//! Tool for generating a nix-index database.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;

use clap::Parser;
use error_chain::ChainedError;
use futures::future::Either;
use futures::{future, StreamExt};
use nix_index::database::Writer;
use nix_index::errors::*;
use nix_index::files::FileTree;
use nix_index::hydra::Fetcher;
use nix_index::listings::{fetch_listings, try_load_paths_cache};
use nix_index::package::StorePath;
use nix_index::CACHE_URL;
use separator::Separatable;

/// The main function of this module: creates a new nix-index database.
async fn update_index(args: &Args) -> Result<()> {
    // first try to load the paths.cache if requested, otherwise query
    // the packages normally. Also fall back to normal querying if the paths.cache
    // fails to load.
    let cached = if args.path_cache {
        eprintln!("+ loading paths from cache");
        try_load_paths_cache()?
    } else {
        None
    };

    eprintln!("+ querying available packages");
    let fetcher = Fetcher::new(CACHE_URL.to_string()).map_err(ErrorKind::ParseProxy)?;
    let (files, watch) = match cached {
        Some((f, w)) => (Either::Left(f), w),
        None => {
            let (f, w) = fetch_listings(
                &fetcher,
                args.jobs,
                &args.nixpkgs,
                vec![args.system.as_deref()],
                args.show_trace,
            )?;
            (Either::Right(f), w)
        }
    };

    // Treat request errors as if the file list were missing
    let files = files.map(|r| {
        r.unwrap_or_else(|e| {
            eprint!("\n{}", e.display_chain());
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

        eprint!("+ generating index: {:05} paths found :: {:05} paths not in binary cache :: {:05} paths in queue \r",
               indexed, missing, watch.queue_len());
        io::stderr().flush().expect("flushing stderr failed");
    });

    // Filter packages with no file listings available
    let mut files = files.filter_map(future::ready);

    eprint!("+ generating index");
    if !args.filter_prefix.is_empty() {
        eprint!(" (filtering by `{}`)", args.filter_prefix);
    }
    eprint!("\r");
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
    eprintln!();

    if args.path_cache {
        eprintln!("+ writing path cache");
        let mut output = io::BufWriter::new(
            File::create("paths.cache").chain_err(|| ErrorKind::WritePathsCache)?,
        );
        bincode::serialize_into(&mut output, &results).chain_err(|| ErrorKind::WritePathsCache)?;
    }

    let index_size = db
        .finish()
        .chain_err(|| ErrorKind::WriteDatabase(args.database.clone()))?;
    eprintln!("+ wrote index of {} bytes", index_size.separated_string());

    Ok(())
}

/// Builds an index for nix-locate
#[derive(Debug, Parser)]
#[clap(author, about, version)]
struct Args {
    /// Make REQUESTS http requests in parallel
    #[clap(short = 'r', long = "requests", default_value = "100")]
    jobs: usize,

    /// Directory where the index is stored
    #[clap(short, long = "db", default_value_os = nix_index::cache_dir(), env = "NIX_INDEX_DATABASE")]
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
        eprintln!("error: {}", e);

        for e in e.iter().skip(1) {
            eprintln!("caused by: {}", e);
        }

        if let Some(backtrace) = e.backtrace() {
            eprintln!("backtrace: {:?}", backtrace);
        }
        process::exit(2);
    }
}
