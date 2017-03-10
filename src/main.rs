#[macro_use] extern crate serde_derive;
#[macro_use] extern crate clap;
extern crate bincode;
extern crate futures;
extern crate lz4;
extern crate nix_index;
extern crate pbr;
extern crate rustc_serialize;
extern crate separator;
extern crate serde;
extern crate serde_json;
extern crate tokio_core;
extern crate tokio_curl;
extern crate xml;
extern crate xz2;
extern crate xdg;
extern crate grep;
extern crate void;

use futures::future;
use futures::{Future, Stream};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write, Seek, SeekFrom};
use std::path::{PathBuf};
use std::process;
use std::str::{self};
use tokio_core::reactor::{Core};
use tokio_curl::Session;
use separator::Separatable;
use clap::{Arg, App, SubCommand, ArgMatches, AppSettings};
use grep::{GrepBuilder};
use void::{ResultVoidExt};

use nix_index::files::{Files};
use nix_index::hydra;
use nix_index::nixpkgs::{self, StorePath};
use nix_index::util;
use nix_index::workset::{WorkSet};

const CACHE_URL: &'static str = "http://cache.nixos.org";

enum Error {
    Io(io::Error),
    QueryPackages(nixpkgs::Error),
    FetchFiles(StorePath, hydra::Error),
    FetchReferences(StorePath, hydra::Error),
    IndexReadError(PathBuf, io::Error),
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
        match self {
            &Io(ref e) => write!(f, "input/output error: {}", e),
            &QueryPackages(ref e) => write!(f, "error while querying available packages: {}", e),
            &FetchFiles(ref path, ref e) => {
                write!(f, "error while fetching the file listing for path {}: {}", path.as_str(), e)
            },
            &FetchReferences(ref path, ref e) => {
                write!(f, "error while fetching references for path {}: {}", path.as_str(), e)
            },
            &IndexReadError(ref path, ref e) => {
                write!(f, "failed to read index at '{}': {}. Have you run update?", path.to_string_lossy(), e)
            }
            &Serialize(ref e) => write!(f, "failed to write output: {}", e),
            &Args(ref e) => write!(f, "{}", e),
        }
    }
}

fn try_fetch_path_files<'a>(session: &'a Session, path: StorePath) ->
    Box<Future<Item=(StorePath, Option<Files>), Error=Error> + 'a>
{
    let fetch_output = {
        util::future_result(|| -> Result<_, Error> {
            let fetch = {
                let path = path.clone();
                util::retry(10, move || hydra::fetch_files(CACHE_URL, session, &path))
            };

            Ok(fetch.then(move |r| future::result(match r {
                Err(e) => Err(Error::FetchFiles(path, e)),
                Ok(files) => Ok((path, files)),
            })))
        })
    };

    Box::new(fetch_output)
}

fn fetch_references<'a>(jobs: usize, session: &'a Session, starting_set: Vec<StorePath>) ->
    Box<Stream<Item=(StorePath, usize), Error=Error> + 'a>
{
    let workset = WorkSet::from_iter(starting_set.into_iter().map(|x| (x.hash().into_owned(), x)));

    let stream = workset
        .then(|r| future::ok(r.void_unwrap()))
        .map(move |(mut handle, path)| {
            let fetch = {
                let path = path.clone();
                util::retry(10, move || hydra::fetch_references(CACHE_URL, session, path.clone()))
            };

            fetch.map_err(move |e| {
                Error::FetchReferences(path.clone(), e)
            }).map(move |(path, references)| {
                for reference in references {
                    let hash = reference.hash().into_owned();
                    handle.add_work(hash, reference);
                }
                (path, handle.queue_len())
            })
        })
        .buffer_unordered(jobs);

    Box::new(stream)
}


struct ArgsCommon {
    jobs: usize,
    database: PathBuf,
}

struct ArgsUpdate {
    common: ArgsCommon,
    nixpkgs: String,
}

fn update_index(args: ArgsUpdate, lp: &mut Core, session: &Session) -> Result<(), Error> {
    writeln!(&mut io::stderr(), "+ querying available packages")?;

    let packages = nixpkgs::query_packages(&args.nixpkgs)?;
    let packages = packages.collect::<Result<Vec<_>, _>>()?;

    let paths: Vec<StorePath> = packages.iter().map(|x| x.0.clone()).collect();

    let requests = fetch_references(args.common.jobs, session, paths.clone()).map(|(path, remaining)| {
        try_fetch_path_files(&session, path).map(move |r| (r, remaining))
    }).buffer_unordered(args.common.jobs);

    let mut indexed = 0;
    fs::create_dir_all(&args.common.database)?;
    let mut file = {
        let output = io::BufWriter::new(File::create(args.common.database.join("files.lz4"))?);
        let mut encoder = lz4::EncoderBuilder::new()
            .block_size(lz4::BlockSize::Max4MB)
            .level(16)
            .build(output)?;
        {
            let encoder = &mut encoder;
            let indexed = &mut indexed;
            lp.run(requests.for_each(move |((path, files), remaining)| {
                future::result((|| {
                    match files {
                        Some(ref files) => {
                            *indexed += 1;
                            for file in files.to_list() {
                                write!(encoder, "{}", path.as_str())?;
                                encoder.write_all(&file.path)?;
                                write!(encoder, "\n")?;
                            }
                        },
                        None => {}
                    }
                    write!(&mut io::stderr(), "\r+ generating index, indexed: {} paths, remaining: {} paths                           \r", indexed, remaining)?;
                    io::stderr().flush()?;
                    Ok(())
                })())
            }))?;
        }

        encoder.finish().0
    };

    writeln!(&mut io::stderr(), "")?;

    let size = file.seek(SeekFrom::Current(0))?;
    writeln!(io::stderr(), "+ wrote index with {} bytes", size.separated_string())?;

    Ok(())
}

struct ArgsLocate {
    common: ArgsCommon,
    pattern: String,
}

fn locate(args: ArgsLocate) -> Result<(), Error> {
    let mut buf = Vec::new();
    let index_file = args.common.database.join("files.lz4");

    (|| {
        let files = File::open(args.common.database.join("files.lz4"))?;
        let mut files = lz4::Decoder::new(files)?;
        files.read_to_end(&mut buf)
    })().map_err(|e| {
        Error::IndexReadError(index_file, e)
    })?;

    let pattern = GrepBuilder::new(&args.pattern).build().unwrap();
    for m in pattern.iter(&buf) {
        let p = &buf[m.start()..m.end()];
        io::stdout().write_all(p)?;
    }

    Ok(())
}

fn run<'a>(matches: ArgMatches<'a>, lp: &mut Core, session: &Session) -> Result<(), Error> {
    let common = ArgsCommon {
        jobs: value_t!(matches.value_of("requests"), usize)?,
        database: PathBuf::from(matches.value_of("database").unwrap()),
    };

    if let Some(matches_update) = matches.subcommand_matches("update") {
        let args_update = ArgsUpdate {
            common: common,
            nixpkgs: matches_update.value_of("nixpkgs").expect("nixpkgs arg required").to_string(),
        };

        return update_index(args_update, lp, session);
    }

    if let Some(matches_locate) = matches.subcommand_matches("locate") {
        let args_locate = ArgsLocate {
            common: common,
            pattern: matches_locate.value_of("PATTERN").expect("pattern arg required").to_string(),
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
                     .default_value("<nixpkgs>")),
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
