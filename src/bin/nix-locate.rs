//! Tool for searching for files in nixpkgs packages
use std::collections::HashSet;
use std::env::args_os;
use std::env::var_os;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::File;
use std::io::stderr;
use std::io::stdout;
use std::io::BufRead;
use std::io::BufReader;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::process::Command;
use std::result;
use std::str;
use std::str::FromStr;

use clap::{value_parser, Parser};
use error_chain::error_chain;
use indoc::writedoc;
use nix_index::database;
use nix_index::files::{self, FileTreeEntry, FileType};
use owo_colors::{OwoColorize, Stream};
use regex::bytes::Regex;
use separator::Separatable;

error_chain! {
    errors {
        ReadDatabase(database: PathBuf) {
            description("database read error")
            display("reading from the database at '{}' failed.\n\
                     This may be caused by a corrupt or missing database, try (re)running `nix-index` to generate the database. \n\
                     If the error persists please file a bug report at https://github.com/nix-community/nix-index.", database.to_string_lossy())
        }
        Grep(pattern: String) {
            description("grep builder error")
            display("constructing the regular expression from the pattern '{}' failed.", pattern)
        }
    }
}

/// The struct holding the parsed arguments for searching
struct Args {
    /// Path of the nix-index database.
    database: PathBuf,
    /// The pattern to search for. This is always in regex syntax.
    pattern: String,
    group: bool,
    hash: Option<String>,
    package_pattern: Option<String>,
    file_type: Vec<FileType>,
    only_toplevel: bool,
    color: bool,
    minimal: bool,
}

/// The main function of this module: searches with the given options in the database.
fn locate(args: &Args) -> Result<()> {
    // Build the regular expression matcher
    let pattern = Regex::new(&args.pattern).chain_err(|| ErrorKind::Grep(args.pattern.clone()))?;
    let package_pattern = if let Some(ref pat) = args.package_pattern {
        Some(Regex::new(pat).chain_err(|| ErrorKind::Grep(pat.clone()))?)
    } else {
        None
    };

    // Open the database
    let index_file = args.database.join("files");
    let db = database::Reader::open(&index_file)
        .chain_err(|| ErrorKind::ReadDatabase(index_file.clone()))?;

    let results = db
        .query(&pattern)
        .package_pattern(package_pattern.as_ref())
        .hash(args.hash.clone())
        .run()
        .chain_err(|| ErrorKind::Grep(args.pattern.clone()))?
        .filter(|v| {
            v.as_ref().ok().map_or(true, |v| {
                let &(ref store_path, FileTreeEntry { ref path, ref node }) = v;
                let m = pattern
                    .find_iter(path)
                    .last()
                    .expect("path should match the pattern");

                let conditions = [
                    !args.group || !path[m.end()..].contains(&b'/'),
                    !args.only_toplevel || store_path.origin().toplevel,
                    args.file_type.iter().any(|t| &node.get_type() == t),
                ];

                conditions.iter().all(|c| *c)
            })
        });

    let mut printed_attrs = HashSet::new();
    for v in results {
        let (store_path, FileTreeEntry { path, node }) =
            v.chain_err(|| ErrorKind::ReadDatabase(index_file.clone()))?;

        use crate::files::FileNode::*;
        let (typ, size) = match node {
            Regular { executable, size } => (if executable { "x" } else { "r" }, size),
            Directory { size, contents: () } => ("d", size),
            Symlink { .. } => ("s", 0),
        };

        let mut attr = format!(
            "{}.{}",
            store_path.origin().attr,
            store_path.origin().output
        );

        if !store_path.origin().toplevel {
            attr = format!("({})", attr);
        }

        if args.minimal {
            // only print each package once, even if there are multiple matches
            if printed_attrs.insert(attr.clone()) {
                println!("{}", attr);
            }
        } else {
            print!(
                "{:<40} {:>14} {:>1} {}",
                attr,
                size.separated_string(),
                typ,
                store_path.as_str()
            );

            let path = String::from_utf8_lossy(&path);

            if args.color {
                let mut prev = 0;
                for mat in pattern.find_iter(path.as_bytes()) {
                    // if the match is empty, we need to make sure we don't use string
                    // indexing because the match may be "inside" a single multibyte character
                    // in that case (for example, the pattern may match the second byte of a multibyte character)
                    if mat.start() == mat.end() {
                        continue;
                    }
                    print!(
                        "{}{}",
                        &path[prev..mat.start()],
                        (&path[mat.start()..mat.end()])
                            .if_supports_color(Stream::Stdout, |txt| txt.red()),
                    );
                    prev = mat.end();
                }
                println!("{}", &path[prev..]);
            } else {
                println!("{}", path);
            }
        }
    }

    Ok(())
}

fn has_env(env: &str) -> bool {
    var_os(env).map_or(false, |var| !var.is_empty())
}

fn has_flakes() -> bool {
    // TODO: user config
    let mut files = vec![PathBuf::from("/etc/nix/nix.conf")];

    while let Some(file) = files.pop() {
        let Ok(file) = File::open(file) else {
            continue;
        };

        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                break;
            };

            let mut tokens = line.split_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };

            match name {
                "experimental-features" => {
                    if tokens.any(|feat| feat == "flakes") {
                        return true;
                    }
                }
                "include" | "!include" => {
                    if let Some(file) = tokens.next() {
                        files.push(file.into());
                    }
                }
                _ => {}
            }
        }
    }

    false
}

fn command_not_found(args: Vec<OsString>) -> Result<()> {
    let mut args = args.into_iter().skip(2);
    let cmd = args.next().expect("there should be a command");
    let cmd_str = cmd.to_string_lossy();
    let database = var_os("NIX_INDEX_DATABASE").map_or_else(|| cache_dir().into(), PathBuf::from);
    let mut err = stderr().lock();

    // TODO: use "command not found" gettext translations

    // taken from http://www.linuxjournal.com/content/bash-command-not-found
    // - do not run when inside Midnight Commander or within a Pipe
    if has_env("MC_SID") || !stdout().is_terminal() {
        let _ = writeln!(err, "{cmd_str}: command not found");
        process::exit(127);
    }

    // Build the regular expression matcher
    let pattern = format!("^/bin/{}$", regex::escape(&cmd_str));
    let regex = Regex::new(&pattern).chain_err(|| ErrorKind::Grep(pattern.clone()))?;

    // Open the database
    let index_file = database.join("files");
    let db = database::Reader::open(&index_file)
        .chain_err(|| ErrorKind::ReadDatabase(index_file.clone()))?;

    let results = db
        .query(&regex)
        .run()
        .chain_err(|| ErrorKind::Grep(pattern.clone()))?
        .filter(|v| {
            v.as_ref().ok().map_or(true, |(store_path, entry)| {
                store_path.origin().toplevel
                    && entry.node.get_type() == FileType::Regular { executable: true }
            })
        });

    let mut attrs = HashSet::new();
    for v in results {
        let (store_path, _) = v.chain_err(|| ErrorKind::ReadDatabase(index_file.clone()))?;

        attrs.insert(format!(
            "{}.{}",
            store_path.origin().attr,
            store_path.origin().output,
        ));
    }

    let mut it = attrs.iter();
    if let Some(attr) = it.next() {
        if it.next().is_some() {
            writedoc! {err, "
                The program '{cmd_str}' is currently not installed. It is provided by;
                several packages. You can install it by typing one of the following:
            "}
            .unwrap();

            let has_flakes = has_flakes();

            for attr in &attrs {
                if has_flakes {
                    writeln!(err, "  nix profile install nixpkgs#{attr}").unwrap();
                } else {
                    writeln!(err, "  nix-env -iA nixpkgs.{attr}").unwrap();
                }
            }

            writeln!(err, "\nOr run it once with:").unwrap();

            for attr in attrs {
                if has_flakes {
                    writeln!(err, "  nix shell nixpkgs#{attr} -c {cmd_str} ...").unwrap();
                } else {
                    writeln!(err, "  nix-shell -p {attr} --run '{cmd_str} ...'").unwrap();
                }
            }
        } else if has_env("NIX_AUTO_INSTALL") {
            writedoc! {err, "
                The program '{cmd_str}' is currently not installed. It is provided by
                the package 'nixpkgs.{attr}', which I will now install for you.
            "}
            .unwrap();

            let res = if has_flakes() {
                Command::new("nix")
                    .arg("profile")
                    .arg("install")
                    .arg(format!("nixpkgs#{attr}"))
                    .status()
            } else {
                Command::new("nix-env")
                    .arg("-iA")
                    .arg(format!("nixpkgs.{attr}"))
                    .status()
            };

            if res.is_ok_and(|status| status.success()) {
                let res = Command::new(cmd).args(args).status();
                if let Ok(status) = res {
                    if let Some(code) = status.code() {
                        process::exit(code);
                    }
                }
            } else {
                writedoc! {err, "
                    Failed to install nixpkgs.{attr}
                    {cmd_str}: command not found
                "}
                .unwrap();
            }
        } else if has_env("NIX_AUTO_RUN") {
            let res = Command::new("nix-build")
                .arg("--no-out-link")
                .arg("-A")
                .arg(attr)
                .arg("<nixpkgs>")
                .status();

            if res.is_ok_and(|status| status.success()) {
                // TODO: escape or find and alternative
                let mut cmd = cmd;
                for arg in args {
                    cmd.push(" ");
                    cmd.push(&arg);
                }

                let res = Command::new("nix-shell")
                    .arg("-p")
                    .arg(attr)
                    .arg("--run")
                    .arg(cmd)
                    .status();

                if let Ok(status) = res {
                    if let Some(code) = status.code() {
                        process::exit(code);
                    }
                }
            } else {
                writedoc! {err, "
                    Failed to install nixpkgs.{attr}
                    {cmd_str}: command not found
                "}
                .unwrap();
            }
        } else {
            let has_flakes = has_flakes();

            writedoc! {err, "
                The program '{cmd_str}' is currently not installed. You can install it
                by typing:
            "}
            .unwrap();

            if has_flakes {
                writeln!(err, "  nix profile install nixpkgs#{attr}").unwrap();
            } else {
                writeln!(err, "  nix-env -iA nixpkgs.{attr}").unwrap();
            }

            writeln!(err, "\nOr run it once with:").unwrap();

            if has_flakes {
                writeln!(err, "  nix shell nixpkgs#{attr} -c {cmd_str} ...").unwrap();
            } else {
                writeln!(err, "  nix-shell -p {attr} --run '{cmd_str} ...'").unwrap();
            }
        }
    } else {
        writeln!(err, "{cmd_str}: command not found").unwrap();
    }

    Ok(())
}

/// Extract the parsed arguments for clap's arg matches.
///
/// Handles parsing the values of more complex arguments.
fn process_args(matches: Opts) -> result::Result<Args, clap::Error> {
    let pattern_arg = matches.pattern;
    let package_arg = matches.package;

    let start_anchor = if matches.at_root { "^" } else { "" };
    let end_anchor = if matches.whole_name { "$" } else { "" };

    let make_pattern = |s: &str, wrap: bool| {
        let regex = if matches.regex {
            s.to_string()
        } else {
            regex::escape(s)
        };
        if wrap {
            format!("{}{}{}", start_anchor, regex, end_anchor)
        } else {
            regex
        }
    };

    let color = match matches.color {
        Color::Auto => atty::is(atty::Stream::Stdout),
        Color::Always => true,
        Color::Never => false,
    };

    let args = Args {
        database: matches.database,
        group: !matches.no_group,
        pattern: make_pattern(&pattern_arg, true),
        package_pattern: package_arg.as_deref().map(|p| make_pattern(p, false)),
        hash: matches.hash,
        file_type: matches
            .r#type
            .unwrap_or_else(|| files::ALL_FILE_TYPES.to_vec()),
        only_toplevel: matches.top_level,
        color,
        minimal: matches.minimal,
    };
    Ok(args)
}

const LONG_USAGE: &str = r#"
How to use
==========

In the simplest case, just run `nix-locate part/of/file/path` to search for all packages that contain
a file matching that path:

$ nix-locate 'bin/firefox'
...all packages containing a file named 'bin/firefox'

Before using this tool, you first need to generate a nix-index database.
Use the `nix-index` tool to do that.

Limitations
===========

* this tool can only find packages which are built by hydra, because only those packages
  will have file listings that are indexed by nix-index

* we can't know the precise attribute path for every package, so if you see the syntax `(attr)`
  in the output, that means that `attr` is not the target package but that it
  depends (perhaps indirectly) on the package that contains the searched file. Example:

  $ nix-locate 'bin/xmonad'
  (xmonad-with-packages.out)      0 s /nix/store/nl581g5kv3m2xnmmfgb678n91d7ll4vv-ghc-8.0.2-with-packages/bin/xmonad

  This means that we don't know what nixpkgs attribute produces /nix/store/nl581g5kv3m2xnmmfgb678n91d7ll4vv-ghc-8.0.2-with-packages,
  but we know that `xmonad-with-packages.out` requires it.
"#;

fn cache_dir() -> &'static OsStr {
    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = Box::new(base.get_cache_home());
    let cache_dir = Box::leak(cache_dir);
    cache_dir.as_os_str()
}

/// Quickly finds the derivation providing a certain file
#[derive(Debug, Parser)]
#[clap(author, about, version, after_help = LONG_USAGE)]
struct Opts {
    /// Pattern for which to search
    // #[clap(name = "PATTERN")]
    pattern: String,

    /// Directory where the index is stored
    #[clap(short, long = "db", default_value_os = cache_dir(), env = "NIX_INDEX_DATABASE")]
    database: PathBuf,

    /// Treat PATTERN as regex instead of literal text. Also applies to NAME.
    #[clap(short, long)]
    regex: bool,

    /// Only print matches from packages whose name matches PACKAGE.
    #[clap(short, long)]
    package: Option<String>,

    /// Only print matches from the package that has the given HASH.
    #[clap(long, name = "HASH")]
    hash: Option<String>,

    /// Only print matches from packages that show up in `nix-env -qa`.
    #[clap(long)]
    top_level: bool,

    /// Only print matches for files that have this type. If the option is given multiple times,
    /// a file will be printed if it has any of the given types.
    /// [options: (r)egular file, e(x)cutable, (d)irectory, (s)ymlink]
    #[clap(short, long, value_parser=value_parser!(FileType))]
    r#type: Option<Vec<FileType>>,

    /// Disables grouping of paths with the same matching part. By default, a path will only be
    /// printed if the pattern matches some part of the last component of the path. For example,
    /// the pattern `a/foo` would match all of `a/foo`, `a/foo/some_file` and `a/foo/another_file`,
    /// but only the first match will be printed. This option disables that behavior and prints
    /// all matches.
    #[clap(long)]
    no_group: bool,

    /// Whether to use colors in output. If auto, only use colors if outputting to a terminal.
    #[clap(long, value_enum, default_value = "auto")]
    color: Color,

    /// Only print matches for files or directories whose basename matches PATTERN exactly.
    /// This means that the pattern `bin/foo` will only match a file called `bin/foo` or
    /// `xx/bin/foo` but not `bin/foobar`.
    #[clap(short, long)]
    whole_name: bool,

    /// Treat PATTERN as an absolute file path, so it only matches starting from the root of a
    /// package. This means that the pattern `/bin/foo` only matches a file called `/bin/foo` or
    /// `/bin/foobar` but not `/libexec/bin/foo`.
    #[clap(long)]
    at_root: bool,

    /// Only print attribute names of found files or directories. Other details such as size or
    /// store path are omitted. This is useful for scripts that use the output of nix-locate.
    #[clap(long)]
    minimal: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Color {
    Always,
    Never,
    Auto,
}

impl FromStr for Color {
    type Err = &'static str;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        match s {
            "always" => Ok(Color::Always),
            "never" => Ok(Color::Never),
            "auto" => Ok(Color::Auto),
            _ => Err(""),
        }
    }
}

fn main() {
    let args: Vec<_> = args_os().collect();
    if matches!(args.get(1), Some(arg) if arg == "--command-not-found") {
        if let Err(e) = command_not_found(args) {
            eprintln!("error: {e}");

            for e in e.iter().skip(1) {
                eprintln!("caused by: {e}");
            }

            if let Some(backtrace) = e.backtrace() {
                eprintln!("backtrace: {backtrace:?}");
            }
        }
        process::exit(127);
    }

    let args = Opts::parse_from(args);
    let args = process_args(args).unwrap_or_else(|e| e.exit());

    if let Err(e) = locate(&args) {
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
