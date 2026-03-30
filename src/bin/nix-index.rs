//! Tool for generating a nix-index database.
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;

use clap::Parser;
use futures::future::Either;
use futures::{future, StreamExt};
use nix_index::database::Writer;
use nix_index::errors::*;
use nix_index::files::FileTree;
use nix_index::hydra::Fetcher;
use nix_index::listings::{self, try_load_paths_cache};
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
    let fetcher = Fetcher::new(CACHE_URL.to_string()).map_err(Error::ParseProxy)?;
    let (files, watch) = match cached {
        Some((f, w)) => (Either::Left(f), w),
        None => {
            let (f, w) = listings::fetch(
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
            eprint!("\n{:?}", e);
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
    fs::create_dir_all(&args.database).map_err(|e| Error::CreateDatabaseDir {
        path: args.database.clone(),
        source: e,
    })?;
    let mut db =
        Writer::create(args.database.join("files"), args.compression_level).map_err(|e| {
            Error::CreateDatabase {
                path: args.database.clone(),
                source: Box::new(e),
            }
        })?;

    let mut results: Vec<(StorePath, String, FileTree)> = Vec::new();
    let mut total_skipped = 0usize;
    let mut packages_with_skipped: Vec<(String, usize)> = Vec::new();
    while let Some(entry) = files.next().await {
        if args.path_cache {
            results.push(entry.clone());
        }
        let (path, _, files) = entry;
        let path_name = path.name().to_string();
        let skipped = db
            .add(path, files, args.filter_prefix.as_bytes())
            .map_err(|e| Error::WriteDatabase {
                path: args.database.clone(),
                source: e,
            })?;
        if skipped > 0 {
            total_skipped += skipped;
            packages_with_skipped.push((path_name, skipped));
        }
    }
    eprintln!();

    // Report skipped entries
    if total_skipped > 0 {
        eprintln!(
            "warning: skipped {} entries with invalid bytes (newlines/NUL in paths)",
            total_skipped.separated_string()
        );
        if args.verbose {
            for (pkg, count) in &packages_with_skipped {
                eprintln!("  - {}: {} entries", pkg, count);
            }
        } else if packages_with_skipped.len() > 5 {
            for (pkg, count) in packages_with_skipped.iter().take(5) {
                eprintln!("  - {pkg}: {count} entries");
            }
            eprintln!(
                "  ... and {} more packages (use --verbose to see all)",
                packages_with_skipped.len() - 5
            );
        } else {
            for (pkg, count) in &packages_with_skipped {
                eprintln!("  - {pkg}: {count} entries");
            }
        }
    }

    if args.path_cache {
        eprintln!("+ writing path cache");
        let mut output = io::BufWriter::new(File::create("paths.cache").map_err(|e| {
            Error::WritePathsCache {
                source: Box::new(e),
            }
        })?);
        bincode::serde::encode_into_std_write(&results, &mut output, bincode::config::standard())
            .map_err(|e| Error::WritePathsCache {
            source: Box::new(e),
        })?;
    }

    let index_size = db.finish().map_err(|e| Error::WriteDatabase {
        path: args.database.clone(),
        source: e,
    })?;
    eprintln!("+ wrote index of {} bytes", index_size.separated_string());

    Ok(())
}

fn cache_dir() -> &'static OsStr {
    let base = xdg::BaseDirectories::with_prefix("nix-index");
    let cache_dir = Box::new(base.get_cache_home().unwrap());
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
    #[clap(short, long = "db", default_value_os = cache_dir(), env = "NIX_INDEX_DATABASE")]
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

    /// Show verbose output (e.g., list all packages with skipped entries)
    #[clap(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if let Err(e) = update_index(&args).await {
        eprintln!("error: {:?}", e);
        process::exit(2);
    }
}
