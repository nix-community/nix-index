#[macro_use]
extern crate clap;
extern crate grep;
extern crate nix_index;
extern crate separator;
extern crate xdg;
extern crate regex;
extern crate isatty;
extern crate ansi_term;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process;
use std::str;
use separator::Separatable;
use clap::{Arg, App, ArgMatches};
use grep::GrepBuilder;
use regex::Regex;
use ansi_term::Colour::Red;

use nix_index::database;
use nix_index::files::{self, FileType, FileTreeEntry};

enum Error {
    Io(io::Error),
    DatabaseRead(database::Error),
    Grep(grep::Error),
    Args(clap::Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<clap::Error> for Error {
    fn from(err: clap::Error) -> Self {
        Error::Args(err)
    }
}

impl From<database::Error> for Error {
    fn from(err: database::Error) -> Self {
        Error::DatabaseRead(err)
    }
}

impl From<grep::Error> for Error {
    fn from(err: grep::Error) -> Self {
        Error::Grep(err)
    }
}

impl From<regex::Error> for Error {
    fn from(err: regex::Error) -> Self {
        Error::Grep(grep::Error::Regex(err))
    }
}


struct Args {
    database: PathBuf,
    pattern: String,
    group: bool,
    hash: Option<String>,
    name_pattern: Option<String>,
    file_type: Vec<FileType>,
    only_toplevel: bool,
    color: bool,
}

fn locate(args: &Args) -> Result<(), Error> {
    let index_file = args.database.join("files.zst");
    let pattern = GrepBuilder::new(&args.pattern).build()?;
    let name_pattern = if let Some(ref pat) = args.name_pattern {
        Some(Regex::new(pat)?)
    } else {
        None
    };

    let mut db = database::Reader::open(index_file)?;

    let results = db.find_iter(&pattern)
        .filter(|v| {
            v.as_ref()
                .ok()
                .map_or(true, |v| {
                    let &(ref store_path, FileTreeEntry { ref path, ref node }) = v;
                    let m = match pattern.regex().find_iter(path).last() {
                        Some(m) => m,
                        None => return false,
                    };

                    let conditions = [
                        !args.group || !path[m.end()..].contains(&b'/'),
                        !args.only_toplevel || (*store_path.origin()).toplevel,
                        args.hash
                            .as_ref()
                            .map_or(true, |h| h == &store_path.hash()),
                        name_pattern
                            .as_ref()
                            .map_or(true, |r| r.is_match(&store_path.name())),
                        args.file_type.iter().any(|t| &node.get_type() == t),
                    ];

                    conditions.iter().all(|c| *c)
                })
        });

    for v in results {
        let (store_path, FileTreeEntry { path, node }) = v?;

        use files::FileNode::*;
        let (typ, size) = match node {
            Regular { executable, size } => (if executable { "x" } else { "r" }, size),
            Directory { size, contents: () }=> ("d", size),
            Symlink { .. } => ("s", 0),
        };

        let mut attr = format!("{}.{}",
                               store_path.origin().attr,
                               store_path.origin().output);
        if !store_path.origin().toplevel {
            attr = format!("({})", attr);
        }

        print!("{:<40} {:>14} {:>1} {}",
               attr,
               size.separated_string(),
               typ,
               store_path.as_str());

        let path = String::from_utf8_lossy(&path);

        if args.color {
            let mut prev = 0;
            for mat in pattern.regex().find_iter(path.as_bytes()) {
                print!("{}{}",
                       &path[prev..mat.start()],
                       Red.paint(&path[mat.start()..mat.end()]));
                prev = mat.end();
            }
            println!("{}", &path[prev..]);
        } else {
            println!("{}", path);
        }
    }

    Ok(())
}

fn run<'a>(matches: &ArgMatches<'a>) -> Result<(), Error> {
    let pattern_arg = matches
        .value_of("PATTERN")
        .expect("pattern arg required")
        .to_string();
    let name_arg = matches.value_of("name");
    let make_pattern = |s: &str| if matches.is_present("regex") {
        s.to_string()
    } else {
        regex::escape(s)
    };
    let color = matches
        .value_of("color")
        .and_then(|x| {
            if x == "auto" {
                return None;
            }
            if x == "always" {
                return Some(true);
            }
            if x == "never" {
                return Some(false);
            }
            unreachable!("color can only be auto, always or never (verified by clap already)")
        });
    let args = Args {
        database: PathBuf::from(matches.value_of("database").expect("database has default value by clap")),
        group: !matches.is_present("no-group"),
        pattern: make_pattern(&pattern_arg),
        name_pattern: name_arg.map(make_pattern),
        hash: matches.value_of("hash").map(str::to_string),
        file_type: matches.values_of("type").map_or(files::ALL_FILE_TYPES.to_vec(), |types| {
            types.map(|t| match t {
                "x" => FileType::Regular { executable: true },
                "r" => FileType::Regular { executable: false },
                "s" => FileType::Symlink,
                "d" => FileType::Directory,
                _ => unreachable!("file type can only be one of x, r, s and d (verified by clap already)"),
            }).collect()
        }),
        only_toplevel: matches.is_present("toplevel"),
        color: color.unwrap_or_else(isatty::stdout_isatty)
    };

    locate(&args)
}

fn main() {
    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = base.get_cache_home();
    let cache_dir = cache_dir.to_string_lossy();

    let matches = App::new("Nixpkgs Files Indexer")
        .version(crate_version!())
        .author(crate_authors!())
        .about("Quickly finds the derivation providing a certain file")
        .arg(Arg::with_name("database")
             .short("d")
             .long("db")
             .default_value(&cache_dir)
             .help("Directory where the index is stored"))
        .arg(Arg::with_name("PATTERN")
             .required(true)
             .help("Pattern for which to search")
             .index(1))
        .arg(Arg::with_name("regex")
             .short("r")
             .long("regex")
             .help("Treat PATTERN as regex instead of literal text. Also applies to --name option."))
        .arg(Arg::with_name("name")
             .short("p")
             .long("package")
             .value_name("PATTERN")
             .help("Only print matches from packages whose name matches PATTERN."))
        .arg(Arg::with_name("hash")
             .long("hash")
             .value_name("HASH")
             .help("Only print matches from the package that has the given HASH."))
        .arg(Arg::with_name("toplevel")
             .long("top-level")
             .help("Only print matches from packages that show up in nix-env -qa."))
        .arg(Arg::with_name("type")
             .short("t")
             .long("type")
             .multiple(true)
             .number_of_values(1)
             .value_name("TYPE")
             .possible_values(&["d", "x", "r", "s"])
             .help("Only print matches for files that have this type.\
                    If the option is given multiple times, a file will be printed if it has any of the given types."
             ))
         .arg(Arg::with_name("no-group")
              .long("no-group")
              .help("Disables grouping of paths with the same matching part. \n\
                     By default, a path will only be printed if the pattern matches some part\n\
                     of the last component of the path. For example, the pattern `a/foo` would\n\
                     match all of `a/foo`, `a/foo/some_file` and `a/foo/another_file`, but only\n\
                     the first match will be printed. This option disables that behavior and prints\n\
                     all matches."
              ))
        .arg(Arg::with_name("color")
             .multiple(false)
             .value_name("COLOR")
             .possible_values(&["always", "never", "auto"])
             .help("Whether to use colors in output. If auto, only use colors if outputting to a terminal.")
        )
        .get_matches();

    run(&matches).unwrap_or_else(|e| {
        use Error::*;
        match e {
            Args(e) => e.exit(),
            Io(e) => writeln!(io::stderr(), "An I/O operation failed: {}", e).unwrap(),
            DatabaseRead(e) => {
                writeln!(io::stderr(), "The database could not be read: {}\n", e).unwrap();
            }
            Grep(e) => {
                writeln!(io::stderr(),
                         "Constructing the regex matcher failed with: {}",
                         e)
                        .unwrap();
            }
        }
        process::exit(2);
    });
}
