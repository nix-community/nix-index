//! Tool for generating a nix-index database.
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::iter;
use std::path::PathBuf;
use std::process;
use std::str;
use std::sync::mpsc::channel;
use std::thread;

use clap::Parser;
use error_chain::ChainedError;
use futures::{future, StreamExt};
use nix_index::database::Writer;
use nix_index::errors::*;
use nix_index::files::FileTree;
use nix_index::hydra::Fetcher;
use nix_index::listings::{fetch_file_listings, try_load_paths_cache};
use nix_index::nixpkgs;
use nix_index::package::StorePath;
use nix_index::CACHE_URL;
use separator::Separatable;
use stderr::*;

/// The URL of the binary cache that we use to fetch file listings and references.
///
/// Hardcoded for now, but may be made a configurable option in the future.
const CACHE_URL: &str = "http://cache.nixos.org";

/// The main function of this module: creates a new nix-index database.
async fn update_index(args: &Args) -> Result<()> {
    // first try to load the paths.cache if requested, otherwise query
    // the packages normally. Also fall back to normal querying if the paths.cache
    // fails to load.
    let cached = if args.path_cache {
        errstln!("+ loading paths from cache");
        try_load_paths_cache()?
    } else {
        None
    };

    let fetcher = Fetcher::new(CACHE_URL.to_string()).map_err(ErrorKind::ParseProxy)?;
    let (files, watch) = match cached {
        Some(v) => v,
        None => {
            // These are the paths that show up in `nix-env -qa`.
            let normal_paths = nixpkgs::query_packages(
                &args.nixpkgs,
                args.system.as_deref(),
                None,
                args.show_trace,
            );

            // We also add some additional sets that only show up in `nix-env -qa -A someSet`.
            //
            // Some of these sets are not build directly by hydra. We still include them here
            // since parts of these sets may be build as dependencies of other packages
            // that are build by hydra. This way, our attribute path information is more
            // accurate.
            //
            // We only need sets that are not marked "recurseIntoAttrs" here, since if they are,
            // they are already part of normal_paths.
            let extra_scopes = [
                "xorg",
                "haskellPackages",
                "rPackages",
                "nodePackages",
                "coqPackages",
            ]
            .iter()
            .map(|scope| {
                nixpkgs::query_packages(
                    &args.nixpkgs,
                    args.system.as_deref(),
                    Some(scope),
                    args.show_trace,
                )
            });

            // Collect results in parallel.
            let rx = {
                let (tx, rx) = channel();
                let handles: Vec<thread::JoinHandle<_>> = iter::once(normal_paths)
                    .chain(extra_scopes)
                    .map(|path_iter| {
                        let tx = tx.clone();
                        thread::spawn(move || {
                            for path in path_iter {
                                tx.send(path).unwrap();
                            }
                        })
                    })
                    .collect();

                for h in handles {
                    h.join().unwrap();
                }

                rx
            };

            let all_paths = rx.iter();

            let paths: Vec<StorePath> = all_paths
                .map(|x| x.chain_err(|| ErrorKind::QueryPackages))
                .collect::<Result<_>>()?;
            fetch_file_listings(&fetcher, args.jobs, paths)
        }
    };

    // Treat request errors as if the file list were missing
    let files = files.map(|r| {
        r.unwrap_or_else(|e| {
            errst!("\n{}", e.display_chain());
            None
        })
    });

    // Add progress output
    let (mut indexed, mut missing) = (0, 0);
    let files = files.inspect(|entry| {
        if entry.is_some() {
            indexed += 1;
        } else {
            missing += 1;
        };

        errst!("+ generating index: {:05} paths found :: {:05} paths not in binary cache :: {:05} paths in queue \r",
               indexed, missing, watch.queue_len());
        io::stderr().flush().expect("flushing stderr failed");
    });

    // Filter packages with no file listings available
    let mut files = files.filter_map(future::ready);

    errst!("+ generating index");
    if !args.filter_prefix.is_empty() {
        errst!(" (filtering by `{}`)", args.filter_prefix);
    }
    errst!("\r");
    fs::create_dir_all(&args.database)
        .chain_err(|| ErrorKind::CreateDatabaseDir(args.database.clone()))?;
    let mut db = Writer::create(args.database.join("files"), args.compression_level)
        .chain_err(|| ErrorKind::CreateDatabase(args.database.clone()))?;

    let mut results: Vec<(StorePath, String, FileTree)> = Vec::new();
    while let Some(entry) = files.next().await {
        if args.path_cache {
            results.push(entry.clone());
        }
        let (path, _, files) = entry;
        db.add(path, files, args.filter_prefix.as_bytes())
            .chain_err(|| ErrorKind::WriteDatabase(args.database.clone()))?;
    }
    errstln!("");

    if args.path_cache {
        errstln!("+ writing path cache");
        let mut output = io::BufWriter::new(
            File::create("paths.cache").chain_err(|| ErrorKind::WritePathsCache)?,
        );
        bincode::serialize_into(&mut output, &results).chain_err(|| ErrorKind::WritePathsCache)?;
    }

    let index_size = db
        .finish()
        .chain_err(|| ErrorKind::WriteDatabase(args.database.clone()))?;
    errstln!("+ wrote index of {} bytes", index_size.separated_string());

    Ok(())
}

fn cache_dir() -> &'static OsStr {
    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = Box::new(base.get_cache_home());
    let cache_dir = Box::leak(cache_dir);
    cache_dir.as_os_str()
}

/// Builds an index for nix-locate
#[derive(Debug, Parser)]
#[clap(author, about, version)]
struct Args {
    /// Make REQUESTS http requests in parallel
    #[clap(short = 'r', long = "requests", default_value = "100")]
    jobs: usize,

    /// Directory where the index is stored
    #[clap(short, long = "db", default_value_os = cache_dir())]
    database: PathBuf,

    /// Path to nixpkgs for which to build the index, as accepted by nix-env -f
    #[clap(short = 'f', long, default_value = "<nixpkgs>")]
    nixpkgs: String,

    /// Specify system platform for which to build the index, accepted by nix-env --argstr system
    #[clap(short = 's', long, value_name = "platform")]
    system: Option<String>,

    /// Zstandard compression level
    #[clap(short, long = "compression", default_value = "22")]
    compression_level: i32,

    /// Show a stack trace in the case of a Nix evaluation error
    #[clap(long)]
    show_trace: bool,

    /// Only add paths starting with PREFIX (e.g. `/bin/`)
    #[clap(long, default_value = "")]
    filter_prefix: String,

    /// Store and load results of fetch phase in a file called paths.cache. This speeds up testing
    /// different database formats / compression.
    ///
    /// Note: does not check if the cached data is up to date! Use only for development.
    #[clap(long)]
    path_cache: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if let Err(e) = update_index(&args).await {
        errln!("error: {}", e);

        for e in e.iter().skip(1) {
            errln!("caused by: {}", e);
        }

        if let Some(backtrace) = e.backtrace() {
            errln!("backtrace: {:?}", backtrace);
        }
        process::exit(2);
    }
}
