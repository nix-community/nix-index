//! Tool for generating a nix-index database.
use clap::Parser;
use error_chain::ChainedError;
use futures::{future, StreamExt};
use nix_index::files::FileNode;
use rusqlite::{Connection, DatabaseName};
use std::ffi::OsString;
use std::io::{self, Write};
use stderr::*;

use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use std::process;

use nix_index::{errors::*, CACHE_URL};

use nix_index::hydra::Fetcher;
use nix_index::listings::fetch_file_listings;
use nix_index::nixpkgs;
use nix_index::package::StorePath;

const EXTRA_SCOPES: [&str; 5] = [
    "xorg",
    "haskellPackages",
    "rPackages",
    "nodePackages",
    "coqPackages",
];

/// The main function of this module: creates a new command-not-found database.
async fn update_index(args: &Args) -> Result<()> {
    let fetcher = Fetcher::new(CACHE_URL.to_string()).map_err(ErrorKind::ParseProxy)?;
    let connection =
        Connection::open_in_memory().map_err(|_| ErrorKind::CreateDatabase(args.output.clone()))?;

    connection
        .execute(
            r#"
        create table Programs (
            name        text not null,
            system      text not null,
            package     text not null,
            primary key (name, system, package)
        );
    "#,
            (),
        )
        .map_err(|_| ErrorKind::CreateDatabase(args.output.clone()))?;

    let debug_connection = Connection::open_in_memory()
        .map_err(|_| ErrorKind::CreateDatabase(args.debug_output.clone()))?;
    debug_connection
        .execute(
            r#"
        create table DebugInfo (
            build_id    text unique not null,
            url         text not null,
            filename    text not null,
            primary key (build_id)
        );
    "#,
            (),
        )
        .map_err(|_| ErrorKind::CreateDatabase(args.debug_output.clone()))?;

    let systems = match &args.systems {
        Some(x) => x.iter().map(|x| Some(x.as_str())).collect(),
        None => vec![None],
    };

    let (files, watch) = {
        errstln!("+ querying available packages");

        let all_paths = systems.into_iter().flat_map(|system| {
            // These are the paths that show up in `nix-env -qa`.
            let normal_paths =
                nixpkgs::query_packages(&args.nixpkgs, system, None, args.show_trace);

            // We also add some additional sets that only show up in `nix-env -qa -A someSet`.
            //
            // Some of these sets are not build directly by hydra. We still include them here
            // since parts of these sets may be build as dependencies of other packages
            // that are build by hydra. This way, our attribute path information is more
            // accurate.
            //
            // We only need sets that are not marked "recurseIntoAttrs" here, since if they are,
            // they are already part of normal_paths.

            normal_paths.chain(EXTRA_SCOPES.iter().flat_map(move |scope| {
                nixpkgs::query_packages(&args.nixpkgs, system, Some(scope), args.show_trace)
            }))
        });

        let paths: Vec<StorePath> = all_paths
            .map(|x| x.chain_err(|| ErrorKind::QueryPackages))
            .collect::<Result<_>>()?;

        fetch_file_listings(&fetcher, args.jobs, paths)
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

    let mut files = files.filter_map(future::ready);

    errst!("+ generating index");
    errst!("\r");

    while let Some((path, nar, files)) = files.next().await {
        let origin = path.origin();

        if !origin.toplevel {
            // skip dependencies
            continue;
        }

        for item in files.to_list(&[]) {
            if let FileNode::Symlink { target: _ } // FIXME: should probably check if the target is executable...
            | FileNode::Regular {
                size: _,
                executable: true,
            } = item.node
            {
                let path = PathBuf::from(OsString::from_vec(item.path));

                if let Ok(binary) = path.strip_prefix("/bin") {
                    let attr = origin.attr.clone();
                    let system = origin.system.clone();
                    let binary: String = binary.to_string_lossy().into();

                    if binary.starts_with('.') || binary.contains('/') || binary.is_empty() {
                        continue;
                    }

                    connection
                        .execute(
                            "insert or replace into Programs(name, system, package) values (?, ?, ?)",
                            (binary, system, attr),
                        )
                        .map_err(|_| ErrorKind::CreateDatabase(args.output.clone()))?;
                }

                if let Ok(debuginfo) = path.strip_prefix("/lib/debug/.build-id") {
                    let build_id: String = debuginfo
                        .to_string_lossy()
                        .replace('/', "")
                        .strip_suffix(".debug")
                        .expect("Debug info files must end with .debug")
                        .into();

                    debug_connection
                        .execute(
                            "insert or replace into DebugInfo(build_id, url, filename) values (?, ?, ?)",
                            (build_id, format!("../{}", nar), path.to_string_lossy().strip_prefix('/')),
                        )
                        .map_err(|_| ErrorKind::CreateDatabase(args.debug_output.clone()))?;
                }
            }
        }
    }
    errstln!("");

    errst!("+ dumping index");

    connection
        .backup(DatabaseName::Main, &args.output, None)
        .map_err(|_| ErrorKind::CreateDatabase(args.output.clone()))?;

    debug_connection
        .backup(DatabaseName::Main, &args.debug_output, None)
        .map_err(|_| ErrorKind::CreateDatabase(args.debug_output.clone()))?;

    Ok(())
}

#[derive(Debug, Parser)]
#[clap(author, about, version)]
struct Args {
    /// Make REQUESTS http requests in parallel
    #[clap(short = 'r', long = "requests", default_value = "100")]
    jobs: usize,

    /// Path to nixpkgs for which to build the index, as accepted by nix-env -f
    #[clap(short = 'f', long, default_value = "<nixpkgs>")]
    nixpkgs: String,

    /// Path for resulting database file
    #[clap(short, long, default_value = "programs.sqlite")]
    output: PathBuf,

    /// Path for debuginfo database file
    #[clap(short, long, default_value = "debug.sqlite")]
    debug_output: PathBuf,

    /// Systems to include in generated database
    #[clap(short = 's', long = "platform")]
    systems: Option<Vec<String>>,

    /// Show a stack trace in the case of a Nix evaluation error
    #[clap(long)]
    show_trace: bool,
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
