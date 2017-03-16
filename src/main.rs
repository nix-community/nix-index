#[macro_use] extern crate serde_derive;
#[macro_use] extern crate clap;
extern crate bincode;
extern crate zstd;
extern crate futures;
extern crate grep;
extern crate lz4;
extern crate nix_index;
extern crate pbr;
extern crate rustc_serialize;
extern crate separator;
extern crate serde;
extern crate serde_json;
extern crate tokio_core;
extern crate tokio_curl;
extern crate tokio_retry;
extern crate tokio_timer;
extern crate void;
extern crate xdg;
extern crate xml;
extern crate xz2;

use futures::future;
use futures::{Future, Stream, IntoFuture};
use std::fmt;
use std::fs::{File};
use std::io::{self, Write};
use std::path::{PathBuf};
use std::process;
use std::collections::{HashMap};
use std::str::{self};
use std::time::{Duration};
use tokio_core::reactor::{Core};
use tokio_curl::Session;
use tokio_retry::{RetryStrategy, RetryError, RetryFuture};
use tokio_retry::strategies::{FixedInterval};
use tokio_timer::{Timer};
use separator::Separatable;
use clap::{Arg, App, SubCommand, ArgMatches, AppSettings};
use grep::{GrepBuilder};
use void::{ResultVoidExt};

use nix_index::database;
use nix_index::files::{self, FileTree, FileTreeEntry};
use nix_index::hydra;
use nix_index::package::{StorePath};
use nix_index::nixpkgs::{self};
use nix_index::util;
use nix_index::workset::{WorkSet, WorkSetHandle};

const CACHE_URL: &'static str = "http://cache.nixos.org";

enum Error {
    Io(io::Error),
    QueryPackages(nixpkgs::Error),
    FetchFiles(StorePath, RetryError<hydra::Error>),
    FetchReferences(StorePath, RetryError<hydra::Error>),
    DatabaseReadError(database::Error),
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

impl From<database::Error> for Error {
    fn from(err: database::Error) -> Self { Error::DatabaseReadError(err) }
}


impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error>{
        use Error::*;
        match self {
            &Io(ref e) => write!(f, "input/output error: {}", e),
            &QueryPackages(ref e) => write!(f, "error while querying available packages: {}", e),
            &FetchFiles(ref path, ref e) => {
                write!(f, "error while fetching the file listing for path {}: {}", path.as_str(), e)
            },
            &FetchReferences(ref path, ref e) => {
                write!(f, "error while fetching references for path {}: {}", path.as_str(), e)
            },
            &Serialize(ref e) => write!(f, "failed to write output: {}", e),
            &DatabaseReadError(ref e) =>
                write!(f, "error while parsing database: {}", e),
            &Args(ref e) => write!(f, "{}", e),
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
        Box<Stream<Item=(WorkSetHandle<String, StorePath>, StorePath), Error=Error> + 'a>
    {
        let workset = WorkSet::from_iter(starting_set.into_iter().map(|x| (x.hash().into_owned(), x)));

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
                        Ok((handle, path))
                    }
                }))
            })
            .buffer_unordered(self.jobs);

        Box::new(stream)
    }
}

struct ArgsCommon {
    jobs: usize,
    database: PathBuf,
}

struct ArgsUpdate {
    common: ArgsCommon,
    nixpkgs: String,
    compression_level: i32,
    path_cache: bool,
}

fn update_index(args: ArgsUpdate, lp: &mut Core, session: &Session) -> Result<(), Error> {
    writeln!(io::stderr(), "+ querying available packages")?;
    let paths: Vec<StorePath> = nixpkgs::query_packages(&args.nixpkgs)?.collect::<Result<_, _>>()?;

    let fetcher = Fetcher {
        session: session,
        timer: Timer::default(),
        retry_strategy: FixedInterval::new(Duration::from_millis(100)).jitter().limit_retries(5),
        jobs: args.common.jobs,
    };

    let requests = fetcher.try_fetch_references(paths.clone()).map(|(handle, path)| {
        fetcher.try_fetch_files(path).map(move |v| (handle, v))
    }).buffer_unordered(args.common.jobs);

    let requests: Box<Stream<Item=_, Error=Error>> = if args.path_cache {
        let mut input = io::BufReader::new(File::open("paths.cache")?);
        let results: Vec<(StorePath, FileTree)> = bincode::deserialize_from(&mut input, bincode::SizeLimit::Infinite)?;
        let mut map: HashMap<String, (StorePath, Option<FileTree>)> = results.into_iter().map(|(path, files)| {
            (path.hash().into_owned(), (path, Some(files)))
        }).collect();
        Box::new(WorkSet::from_iter(map.clone().into_iter().map(|(k, (path, _))| (k, path))).then(move |r| {
            let (handle, key) = r.void_unwrap();
            future::ok((handle, map.remove(key.hash().as_ref()).unwrap()))
        }))
    } else { Box::new(requests) };

    let (mut indexed, mut missing) = (0, 0);
    let requests = requests.filter_map(|(handle, entry)| {
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
               indexed, missing, handle.queue_len()).expect("writing to stderr failed");
        io::stderr().flush().expect("flushing stderr failed");

        r
    });

    write!(io::stderr(), "+ generating index\r")?;

    let mut db = database::Writer::create(args.common.database.join("files.zst"), args.compression_level)?;

    let mut results: Vec<(StorePath, FileTree)> = Vec::new();
    lp.run(requests.for_each(|entry| {
        results.push(entry.clone());
        let mut process = |(path, files)| -> Result<_, Error> {
            db.add(path, files)?;
            Ok(())
        };
        future::result(process(entry))
    }))?;
    writeln!(&mut io::stderr(), "")?;

    writeln!(io::stderr(), "+ writing path cache")?;
    let mut output = io::BufWriter::new(File::create("paths.cache")?);
    bincode::serialize_into(&mut output, &results, bincode::SizeLimit::Infinite)?;

    let index_size = db.finish()?;
    writeln!(io::stderr(), "+ wrote index of {} bytes", index_size.separated_string())?;

    Ok(())
}

struct ArgsLocate {
    common: ArgsCommon,
    pattern: String,
}

fn locate(args: ArgsLocate) -> Result<(), Error> {
    let index_file = args.common.database.join("files.zst");
    let pattern = GrepBuilder::new(&args.pattern).build().unwrap();

    let mut db = database::Reader::open(index_file)?;

    for v in db.find_iter(&pattern) {
        let (store_path, FileTreeEntry { path, node }) = v?;
        let m = pattern.regex().find(&path).expect("path matches pattern");
        if path[m.end()..].contains(&b'/') { continue }

        use files::FileNode::*;
        let (t, s) = match node {
            Regular { executable, size } => (if executable { "X" } else { "R" }, size),
            Directory { size, contents: () }=> ("D", size),
            Symlink { .. } => ("S", 0),
        };

        let mut desc = format!("{}.{}", store_path.origin().attr, store_path.origin().output);
        if !store_path.origin().toplevel {
            desc = format!("({})", desc);
        }

        print!("{:>1} {:<40} {:<40} {:>14} ", t, desc, store_path.name(), s.separated_string());

        io::stdout().write_all(&path)?;
        io::stdout().write_all(b"\n")?;
    }

    Ok(())
}

fn run<'a>(matches: ArgMatches<'a>, lp: &mut Core, session: &Session) -> Result<(), Error> {
    let common = ArgsCommon {
        jobs: value_t!(matches.value_of("requests"), usize)?,
        database: PathBuf::from(matches.value_of("database").unwrap()),
    };

    if let Some(matches) = matches.subcommand_matches("update") {
        let args_update = ArgsUpdate {
            common: common,
            nixpkgs: matches.value_of("nixpkgs").expect("nixpkgs arg required").to_string(),
            compression_level: value_t!(matches.value_of("level"), i32)?,
            path_cache: matches.is_present("pathcache"),
        };

        return update_index(args_update, lp, session);
    }

    if let Some(matches) = matches.subcommand_matches("locate") {
        let args_locate = ArgsLocate {
            common: common,
            pattern: matches.value_of("PATTERN").expect("pattern arg required").to_string(),
        };

        return locate(args_locate);
    }

    println!("{}", matches.usage());
    Ok(())
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
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .setting(AppSettings::VersionlessSubcommands)
        .about("Quickly finds the derivation providing a certain file")
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
        .subcommands( vec![
            SubCommand::with_name("update")
                .about("Generates the index used for queries")
                .display_order(2)
                .arg(Arg::with_name("nixpkgs")
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
                     .help("Cache paths and file listings in paths.cache (for development only, speeds up testing different database formats)")),
            SubCommand::with_name("locate")
                .display_order(1)
                .about("Locates a file matching a regex")
                .arg(Arg::with_name("PATTERN")
                     .required(true)
                     .help("Regex for which to search")
                     .index(1))
        ])
        .get_matches();

    run(matches, &mut lp, &session).unwrap_or_else(|e| {
        if let Error::Args(e) = e {
            e.exit()
        }
        writeln!(&mut io::stderr(), "{}", e).unwrap();
        process::exit(2);
    });
}
