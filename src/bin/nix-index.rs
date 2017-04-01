#[macro_use] extern crate clap;
extern crate bincode;
extern crate futures;
extern crate nix_index;
extern crate separator;
extern crate tokio_core;
extern crate tokio_curl;
extern crate tokio_retry;
extern crate tokio_timer;
extern crate void;
extern crate xdg;

use futures::future;
use futures::{Future, Stream, IntoFuture};
use std::fmt;
use std::fs::{File};
use std::io::{self, Write};
use std::path::{PathBuf};
use std::process;
use std::str::{self};
use std::iter::{FromIterator};
use std::time::{Duration};
use tokio_core::reactor::{Core};
use tokio_curl::Session;
use tokio_retry::{RetryStrategy, RetryError, RetryFuture};
use tokio_retry::strategies::{FixedInterval};
use tokio_timer::{Timer};
use separator::Separatable;
use clap::{Arg, App, ArgMatches};
use void::{ResultVoidExt};

use nix_index::database;
use nix_index::files::{FileTree};
use nix_index::hydra;
use nix_index::package::{StorePath};
use nix_index::nixpkgs::{self};
use nix_index::util;
use nix_index::workset::{WorkSet, WorkSetWatch};

const CACHE_URL: &'static str = "http://cache.nixos.org";

enum Error {
    Io(io::Error),
    QueryPackages(nixpkgs::Error),
    FetchFiles(StorePath, RetryError<hydra::Error>),
    FetchReferences(StorePath, RetryError<hydra::Error>),
    Serialize(bincode::Error),
    Args(clap::Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self { Error::Io(err) }
}

impl From<nixpkgs::Error> for Error {
    fn from(err: nixpkgs::Error) -> Self { Error::QueryPackages(err) }
}

impl From<bincode::Error> for Error {
    fn from(err: bincode::Error) -> Self { Error::Serialize(err) }
}

impl From<clap::Error> for Error {
    fn from(err: clap::Error) -> Self { Error::Args(err) }
}


impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error>{
        use Error::*;
        match *self {
            Io(ref e) => write!(f, "input/output error: {}", e),
            QueryPackages(ref e) => write!(f, "error while querying available packages: {}", e),
            FetchFiles(ref path, ref e) => {
                write!(f, "error while fetching the file listing for path {}: {}", path.as_str(), e)
            },
            FetchReferences(ref path, ref e) => {
                write!(f, "error while fetching references for path {}: {}", path.as_str(), e)
            },
            Serialize(ref e) => write!(f, "failed to serialize output: {}", e),
            Args(ref e) => write!(f, "{}", e),
        }
    }
}

struct Fetcher<'a, S> {
    session: &'a Session,
    timer: Timer,
    retry_strategy: S,
    jobs: usize,
}

impl<'a, S> Fetcher<'a, S> where
    S: Clone + RetryStrategy,
{
    fn retry<F: FnMut() -> A, A: IntoFuture>(&self, f: F) -> RetryFuture<S, A, F> {
        self.retry_strategy.clone().run(self.timer.clone(), f)
    }

    fn try_fetch_files(&'a self, path: StorePath) ->
        Box<Future<Item=(StorePath, Option<FileTree>), Error=Error> + 'a>
    {
        let fetch_output = {
            util::future_result(|| -> Result<_, Error> {
                let fetch = {
                    let path = path.clone();
                    self.retry(move || hydra::fetch_files(CACHE_URL, self.session, &path))
                };

                Ok(fetch.then(move |r| future::result(match r {
                    Err(e) => Err(Error::FetchFiles(path, e)),
                    Ok(files) => Ok((path, files)),
                })))
            })
        };

        Box::new(fetch_output)
    }


    fn try_fetch_references(&'a self, starting_set: Vec<StorePath>) ->
        (Box<Stream<Item=StorePath, Error=Error> + 'a>, WorkSetWatch)
    {
        let workset = WorkSet::from_iter(starting_set.into_iter().map(|x| (x.hash().into_owned(), x)));
        let watch = workset.watch();

        let stream = workset
            .then(|r| future::ok(r.void_unwrap()))
            .map(move |(mut handle, path)| {
                let fetch = {
                    let path = path.clone();
                    self.retry(move || hydra::fetch_references(CACHE_URL, self.session, path.clone()))
                };

                fetch.then(move |e| future::result(match e {
                    Err(e) => Err(Error::FetchReferences(path, e)),
                    Ok((path, references)) =>  {
                        for reference in references {
                            let hash = reference.hash().into_owned();
                            handle.add_work(hash, reference);
                        }
                        Ok(path)
                    }
                }))
            })
            .buffer_unordered(self.jobs);

        (Box::new(stream), watch)
    }
}

type PackageStream = (Box<Stream<Item=(StorePath, Option<FileTree>), Error=Error>>, WorkSetWatch);

fn try_load_paths_cache() ->
    Result<Option<PackageStream>, Error>
{
    let file = match File::open("paths.cache") {
        Ok(file) => file,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::from(e)),
    };

    let mut input = io::BufReader::new(file);
    let fetched: Vec<(StorePath, FileTree)> = bincode::deserialize_from(&mut input, bincode::Infinite)?;
    let workset = WorkSet::from_iter(fetched.into_iter().map(|(path, tree)| (path.hash().to_string(), (path, Some(tree)))));
    let watch = workset.watch();
    let stream = workset.then(|r| {
        let (_handle, v) = r.void_unwrap();
        future::ok(v)
    });

    Ok(Some((Box::new(stream), watch)))
}

struct Args {
    jobs: usize,
    database: PathBuf,
    nixpkgs: String,
    compression_level: i32,
    path_cache: bool,
}

fn update_index(args: &Args, lp: &mut Core, session: &Session) -> Result<(), Error> {
    writeln!(io::stderr(), "+ querying available packages")?;
    let paths: Vec<StorePath> = nixpkgs::query_packages(&args.nixpkgs)?.collect::<Result<_, _>>()?;

    let fetcher = Fetcher {
        session: session,
        timer: Timer::default(),
        retry_strategy: FixedInterval::new(Duration::from_millis(100)).jitter().limit_retries(5),
        jobs: args.jobs,
    };

    let (requests, watch) = fetcher.try_fetch_references(paths.clone());
    let requests = Box::new(requests.map(|path| {
        fetcher.try_fetch_files(path)
    }).buffer_unordered(args.jobs));

    let cached = if args.path_cache {
        try_load_paths_cache()?
    } else {
        None
    };

    let (requests, watch) = cached.unwrap_or((requests, watch));

    let (mut indexed, mut missing) = (0, 0);
    let requests = requests.filter_map(move |entry| {
        let (path, files) = entry;

        let r = if let Some(files) = files {
            indexed += 1;
            Some((path, files))
        } else {
            missing += 1;
            None
        };

        write!(io::stderr(),
               "+ generating index: {:05} paths found :: {:05} paths not in binary cache :: {:05} paths in queue \r",
               indexed, missing, watch.queue_len()).expect("writing to stderr failed");
        io::stderr().flush().expect("flushing stderr failed");

        r
    });

    write!(io::stderr(), "+ generating index\r")?;

    let mut db = database::Writer::create(args.database.join("files.zst"), args.compression_level)?;

    let mut results: Vec<(StorePath, FileTree)> = Vec::new();
    lp.run(requests.for_each(|entry| {
        if args.path_cache {
            results.push(entry.clone());
        }
        let mut process = |(path, files)| -> Result<_, Error> {
            db.add(path, files)?;
            Ok(())
        };
        future::result(process(entry))
    }))?;
    writeln!(&mut io::stderr(), "")?;

    if args.path_cache {
        writeln!(io::stderr(), "+ writing path cache")?;
        let mut output = io::BufWriter::new(File::create("paths.cache")?);
        bincode::serialize_into(&mut output, &results, bincode::Infinite)?;
    }

    let index_size = db.finish()?;
    writeln!(io::stderr(), "+ wrote index of {} bytes", index_size.separated_string())?;

    Ok(())
}

fn run<'a>(matches: &ArgMatches<'a>, lp: &mut Core, session: &Session) -> Result<(), Error> {
    let args = Args {
        jobs: value_t!(matches.value_of("requests"), usize)?,
        database: PathBuf::from(matches.value_of("database").unwrap()),
        nixpkgs: matches.value_of("nixpkgs").expect("nixpkgs arg required").to_string(),
        compression_level: value_t!(matches.value_of("level"), i32)?,
        path_cache: matches.is_present("pathcache"),
    };

    
    update_index(&args, lp, session)
}

fn main() {
    let mut lp = Core::new().unwrap();
    let session = Session::new(lp.handle());

    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = base.get_cache_home();
    let cache_dir = cache_dir.to_string_lossy();

    let matches = App::new("Nixpkgs Files Indexer")
        .version(crate_version!())
        .author(crate_authors!())
        .about("Build an index for nix-locate.")
        .arg(Arg::with_name("requests")
             .short("r")
             .long("requests")
             .value_name("NUM")
             .help("make NUM http requests in parallel")
             .default_value("100"))
        .arg(Arg::with_name("database")
             .short("d")
             .long("db")
             .default_value(&cache_dir)
             .help("Directory where the index is stored"))
        .arg(Arg::with_name("nixpkgs")
             .short("f")
             .long("nixpkgs")
             .help("Path to nixpgs for which to build the index, as accepted by nix-env -f")
             .default_value("<nixpkgs>"))
        .arg(Arg::with_name("level")
             .short("c")
             .long("compression")
             .help("Zstandard compression level")
             .default_value("22"))
        .arg(Arg::with_name("pathcache")
             .long("path-cache")
             .hidden(true)
             .help("Store and load results of fetch phase in a file called paths.cache.\n\
                    This speeds up testing different database formats / compression.\n\
                    Note: does not check if the cached data is up to date! Use only for development."))
        .get_matches();

    run(&matches, &mut lp, &session).unwrap_or_else(|e| {
        if let Error::Args(e) = e {
            e.exit()
        }
        writeln!(&mut io::stderr(), "{}", e).unwrap();
        process::exit(2);
    });
}
