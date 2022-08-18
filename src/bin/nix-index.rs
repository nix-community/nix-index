//! Tool for generating a nix-index database.
#[macro_use]
extern crate clap;
extern crate bincode;
extern crate futures;
extern crate hyper;
extern crate nix_index;
extern crate separator;
extern crate tokio_retry;
extern crate void;
extern crate xdg;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate stderr;

use clap::{App, Arg, ArgMatches};
use error_chain::ChainedError;
use futures::{future, FutureExt, Stream, StreamExt, TryFutureExt};
use separator::Separatable;
use std::fs::{self, File};
use std::io::{self, Write};
use std::iter::FromIterator;
use std::path::PathBuf;
use std::pin::Pin;
use std::process;
use std::result;
use std::str;

use nix_index::database;
use nix_index::files::FileTree;
use nix_index::hydra::Fetcher;
use nix_index::nixpkgs;
use nix_index::package::StorePath;
use nix_index::workset::{WorkSet, WorkSetHandle, WorkSetWatch};

/// The URL of the binary cache that we use to fetch file listings and references.
///
/// Hardcoded for now, but may be made a configurable option in the future.
const CACHE_URL: &'static str = "http://cache.nixos.org";

error_chain! {
    errors {
        QueryPackages {
            description("query packages error")
            display("querying available packages failed")
        }
        FetchFiles(path: StorePath) {
            description("file listing fetch error")
            display("fetching the file listing for store path '{}' failed", path.as_str())
        }
        FetchReferences(path: StorePath) {
            description("references fetch error")
            display("fetching the references of store path '{}' failed", path.as_str())
        }
        LoadPathsCache {
            description("paths.cache load error")
            display("loading the paths.cache file failed")
        }
        WritePathsCache {
            description("paths.cache write error")
            display("writing the paths.cache file failed")
        }
        CreateDatabase(path: PathBuf) {
            description("crate database error")
            display("creating the database at '{}' failed", path.to_string_lossy())
        }
        CreateDatabaseDir(path: PathBuf) {
            description("crate database directory error")
            display("creating the directory for the database at '{}' failed", path.to_string_lossy())
        }
        WriteDatabase(path: PathBuf) {
            description("database write error")
            display("writing to the database '{}' failed", path.to_string_lossy())
        }
        ParseProxy(err: nix_index::hydra::Error){
            description("proxy parse error")
            display("Can not parse proxy settings")
        }
    }
}

/// A stream of store paths (packages) with their associated file listings.
///
/// If a store path has no file listing (for example, because it is not built by hydra),
/// the file listing will be `None` instead.
type FileListingStream<'a> =
    Pin<Box<dyn Stream<Item = Result<Option<(StorePath, FileTree)>>> + 'a>>;

/// Fetches all the file listings for the full closure of the given starting set of path.
///
/// This function will fetch the file listings of each path in the starting set. Additionally, it
/// will also determine the references of each path and recursively fetch the file listings for those
/// paths.
///
/// The `jobs` argument is used to specify how many requests should be done in parallel. No more than
/// `jobs` requests will be in-flight at any given time.
fn fetch_file_listings(
    fetcher: &Fetcher,
    jobs: usize,
    starting_set: Vec<StorePath>,
) -> (FileListingStream, WorkSetWatch) {
    // Create the queue that will hold all the paths that still need processing.
    // Initially, only the starting set needs processing.
    let workset = WorkSet::from_iter(starting_set.into_iter().map(|x| (x.hash().into_owned(), x)));

    // Processes a single store path, fetching the file listing for it and
    // adding its references to the queue
    let process = move |mut handle: WorkSetHandle<_, _>, path: StorePath| {
        fetcher
            .fetch_references(path.clone())
            .map_err(|e| Error::with_chain(e, ErrorKind::FetchReferences(path)))
            .and_then(move |(path, references)| match references {
                Some(references) => {
                    for reference in references {
                        let hash = reference.hash().into_owned();
                        handle.add_work(hash, reference);
                    }
                    future::Either::Left(fetcher.fetch_files(&path).map(move |r| match r {
                        Err(e) => Err(Error::with_chain(e, ErrorKind::FetchFiles(path))),
                        Ok(Some(files)) => Ok(Some((path, files))),
                        Ok(None) => Ok(None),
                    }))
                }
                None => future::Either::Right(future::ok(None)),
            })
    };

    // Process all paths in the queue, until the queue becomes empty.
    let watch = workset.watch();
    let stream = workset
        .map(move |(handle, path)| process(handle, path))
        .buffer_unordered(jobs);
    (Box::pin(stream), watch)
}

/// Tries to load the file listings for all paths from a cache file named `paths.cache`.
///
/// This function is used to implement the `--path-cache` option.
fn try_load_paths_cache() -> Result<Option<(FileListingStream<'static>, WorkSetWatch)>> {
    let file = match File::open("paths.cache") {
        Ok(file) => file,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).chain_err(|| ErrorKind::LoadPathsCache)?,
    };

    let mut input = io::BufReader::new(file);
    let fetched: Vec<(StorePath, FileTree)> =
        bincode::deserialize_from(&mut input).chain_err(|| ErrorKind::LoadPathsCache)?;
    let workset = WorkSet::from_iter(
        fetched
            .into_iter()
            .map(|(path, tree)| (path.hash().to_string(), Some((path, tree)))),
    );
    let watch = workset.watch();
    let stream = workset.map(|r| {
        let (_handle, v) = r;
        Ok(v)
    });

    Ok(Some((Box::pin(stream), watch)))
}

/// A struct holding the processed arguments for database creation.
struct Args {
    jobs: usize,
    database: PathBuf,
    nixpkgs: String,
    system: Option<String>,
    compression_level: i32,
    path_cache: bool,
    show_trace: bool,
    filter_prefix: String,
}

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

    let fetcher = Fetcher::new(CACHE_URL.to_string()).map_err(|e| ErrorKind::ParseProxy(e))?;
    let (files, watch) = match cached {
        Some(v) => v,
        None => {
            errstln!("+ querying available packages");
            // These are the paths that show up in `nix-env -qa`.
            let normal_paths = nixpkgs::query_packages(&args.nixpkgs, args.system.as_deref(), None, args.show_trace);

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
            ];

            let all_paths = normal_paths.chain(extra_scopes.iter().flat_map(|scope| {
                nixpkgs::query_packages(&args.nixpkgs, args.system.as_deref(), Some(scope), args.show_trace)
            }));

            let paths: Vec<StorePath> = all_paths
                .map(|x| x.chain_err(|| ErrorKind::QueryPackages))
                .collect::<Result<_>>()?;
            fetch_file_listings(&fetcher, args.jobs, paths.clone())
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
    let mut files = files.filter_map(|entry| future::ready(entry));

    errst!("+ generating index");
    if args.filter_prefix.len() > 0 {
        errst!(" (filtering by `{}`)", args.filter_prefix);
    }
    errst!("\r");
    fs::create_dir_all(&args.database)
        .chain_err(|| ErrorKind::CreateDatabaseDir(args.database.clone()))?;
    let mut db = database::Writer::create(args.database.join("files"), args.compression_level)
        .chain_err(|| ErrorKind::CreateDatabase(args.database.clone()))?;

    let mut results: Vec<(StorePath, FileTree)> = Vec::new();
    while let Some(entry) = files.next().await {
        if args.path_cache {
            results.push(entry.clone());
        }
        let (path, files) = entry;
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

/// Extract the arguments from clap's arg matches, applying defaults and parsing them
/// where necessary.
fn process_args(matches: &ArgMatches) -> result::Result<Args, clap::Error> {
    let args = Args {
        jobs: value_t!(matches.value_of("requests"), usize)?,
        database: PathBuf::from(matches.value_of("database").unwrap()),
        nixpkgs: matches
            .value_of("nixpkgs")
            .expect("nixpkgs arg required")
            .to_string(),
        system: matches.value_of("system").map(|s| s.to_string()),
        compression_level: value_t!(matches.value_of("level"), i32)?,
        path_cache: matches.is_present("path-cache"),
        show_trace: matches.is_present("show-trace"),
        filter_prefix: matches.value_of("filter-prefix").unwrap_or("").to_string(),
    };

    Ok(args)
}

#[tokio::main]
async fn main() {
    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = base.get_cache_home();
    let cache_dir = cache_dir.to_string_lossy();

    let matches = App::new("Nixpkgs Files Indexer")
        .version(crate_version!())
        .author(crate_authors!())
        .about("Builds an index for nix-locate.")
        .arg(Arg::with_name("requests")
            .short("r")
            .long("requests")
            .value_name("NUM")
            .help("Make NUM http requests in parallel")
            .default_value("100"))
        .arg(Arg::with_name("database")
            .short("d")
            .long("db")
            .default_value(&cache_dir)
            .help("Directory where the index is stored"))
        .arg(Arg::with_name("nixpkgs")
            .short("f")
            .long("nixpkgs")
            .help("Path to nixpkgs for which to build the index, as accepted by nix-env -f")
            .default_value("<nixpkgs>"))
        .arg(Arg::with_name("system")
            .long("system")
            .value_name("platform")
            .takes_value(true)
            .help("Specify system platform for which to build the index, accepted by nix-env --argstr system"))
        .arg(Arg::with_name("level")
            .short("c")
            .long("compression")
            .help("Zstandard compression level")
            .default_value("22"))
        .arg(Arg::with_name("show-trace")
            .long("show-trace")
            .help("Show a stack trace in case of Nix expression evaluation errors"))
        .arg(Arg::with_name("filter-prefix")
            .long("filter-prefix")
            .value_name("PREFIX")
            .help("Only add paths starting with PREFIX (e.g. `/bin/`)"))
        .arg(Arg::with_name("path-cache")
             .long("path-cache")
             .hidden(true)
             .help("Store and load results of fetch phase in a file called paths.cache.\n\
                    This speeds up testing different database formats / compression.\n\
                    Note: does not check if the cached data is up to date! Use only for development."))
        .get_matches();

    let args = process_args(&matches).unwrap_or_else(|e| e.exit());

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
