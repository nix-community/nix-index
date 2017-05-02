//! command-not-found handler for nix-locate
#[macro_use]
extern crate clap;
extern crate grep;
extern crate nix_index;
extern crate separator;
extern crate xdg;
extern crate regex;
extern crate isatty;
extern crate ansi_term;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate stderr;

use std::path::PathBuf;
use std::result;
use std::process;
use std::str;
use clap::{Arg, App, ArgMatches};
use grep::GrepBuilder;

use nix_index::database;

error_chain! {
    errors {
        ReadDatabase(database: PathBuf) {
            description("database read error")
            display("reading from the database at '{}' failed", database.to_string_lossy())
        }
        Grep(pattern: String) {
            description("grep builder error")
            display("constructing the regular expression from the pattern '{}' failed", pattern)
        }
    }
}

/// The struct holding the parsed arguments for searching
struct Args {
    /// Path of the nix-index database.
    database: PathBuf,
    /// The command that wasn’t found
    command: String,
}

/// The main function of this module: searches with the given options in the database.
fn locate(args: &Args) -> Result<()> {
    // Build the regular expression matcher
    let command = "bin/".to_string() + &args.command + "$";
    let pattern = GrepBuilder::new(&command).build().chain_err(|| ErrorKind::Grep(command.clone()))?;

    // Open the database
    let index_file = args.database.join("files");
    let mut db = database::Reader::open(&index_file).chain_err(|| ErrorKind::ReadDatabase(index_file.clone()))?;

    let results = db.find_iter(&pattern)
        .filter(|v| {
            v.as_ref()
                .ok()
                .map_or(true, |v| {
                    let &(ref store_path, ..) = v;

                    let conditions = [
                        (*store_path.origin()).toplevel,
                    ];

                    conditions.iter().all(|c| *c)
                })
        });


    let mut attrs = Vec::new();

    for v in results {
        let (store_path, ..) = v.chain_err(|| ErrorKind::ReadDatabase(index_file.clone()))?;

        let attr = format!("{}.{}", store_path.origin().attr, store_path.origin().output);

        attrs.push(attr);
    }

    match attrs.len() {
        0 => errln!("{}: command not found", args.command),
        1 => errln!("The program ‘{}’ is currently not installed. You can install it
by typing:
", args.command),
        _ => errln!("The program ‘{}’ is currently not installed. It is provided by
several packages. You can install it by typing one of the following:
", args.command),
    }

    for attr in attrs {
        // TODO: How to tell whether nixpkgs or nixos?
        errln!("  nix-env -iA nixos.{}", attr);
    }

    // TODO: Implement "auto-run" like command-not-found.pl

    Ok(())
}

fn process_args(matches: &ArgMatches) -> result::Result<Args, clap::Error> {
    let cmd_arg = matches
        .value_of("COMMAND")
        .expect("command arg required")
        .to_string();
    let args = Args {
        database: PathBuf::from(matches.value_of("database").expect("database has default value by clap")),
        command: cmd_arg.to_string(),
    };
    Ok(args)
}

fn main() {
    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = base.get_cache_home();
    let cache_dir = cache_dir.to_string_lossy();

    let matches = App::new("Command Not Found")
        .version(crate_version!())
        .author(crate_authors!())
        .about("command-not-found hook for bash or zsh")
        .arg(Arg::with_name("database")
             .short("d")
             .long("db")
             .default_value(&cache_dir)
             .help("Directory where the index is stored"))
        .arg(Arg::with_name("COMMAND")
             .required(true)
             .help("Command that is unavailable")
             .index(1))
        .get_matches();

    let args = process_args(&matches).unwrap_or_else(|e| e.exit());

    match locate(&args) {
        Err(Error(ErrorKind::ReadDatabase(_), _)) => {
            errln!("{}: command not found", args.command);
            process::exit(127);
        },
        Err(e) => {
            errln!("error: {}", e);

            for e in e.iter().skip(1) {
                errln!("caused by: {}", e);
            }

            if let Some(backtrace) = e.backtrace() {
                errln!("backtrace: {:?}", backtrace);
            }
            process::exit(2);
        },
        Ok(_) => process::exit(127), // command not found error code
    }
}
