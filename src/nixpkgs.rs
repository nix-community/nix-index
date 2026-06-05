//! Read package information from nix-env.
//!
//! This module implements the gathering of initial set of root store paths to fetch.
//! We parse the output `nix-env --query` to figure out all accessible store paths with their attribute path
//! and hashes.
use std::error;
use std::fmt;
use std::io::{self, BufReader};
use std::process::{Command, Stdio};

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::package::{PathOrigin, StorePath};

/// Calls `nix-env` to list the packages in the given nixpkgs.
///
/// The `nixpkgs` argument can either be a path to a nixpkgs checkout or another expression
/// accepted by `nix-env -f`, such as `<nixpkgs>` or `http://example.org/nixpkgs.tar.bz`.
///
/// If system is `Some(platform)`, nix-env is called with the `--argstr system <platform>` argument so that
/// the specified platform would be used instead of the default host system platform.
///
/// If scope is `Some(attr)`, nix-env is called with the `-A attr` argument so only packages that are a member
/// of `attr` are returned.
///
/// The function returns an [`IntoIterator`] over the packages returned by nix-env.
pub fn query_packages(
    nixpkgs: &str,
    system: Option<&str>,
    scope: Option<&str>,
    show_trace: bool,
) -> Result<Packages, Error> {
    let mut cmd = Command::new("nix-env");
    cmd.arg("-qaP")
        .arg("--out-path")
        .arg("--xml")
        .arg("--arg")
        .arg("config")
        .arg("{ allowAliases = false; }") // override default nixpkgs config discovery
        .arg("--arg")
        .arg("overlays")
        .arg("[ ]")
        .arg("--file")
        .arg(nixpkgs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    if let Some(system) = system {
        cmd.arg("--argstr").arg("system").arg(system);
    }

    if let Some(scope) = scope {
        cmd.arg("-A").arg(scope);
    }

    if show_trace {
        cmd.arg("--show-trace");
    }

    run(cmd)
}

/// An [`IntoIterator`] of parsed store paths from the output of nix-env.
///
/// Use `query_packages` to create a value of this type.
pub type Packages = Vec<StorePath>;

/// Spawns the nix-env subprocess and runs the parser,
/// waits for it to exit, and checks whether it has returned a non-zero exit code
/// (= failed with an error).
///
/// If the exit code was non-zero, returns `Err(err)`, else it returns `Ok`.
fn run(mut cmd: Command) -> Result<Packages, Error> {
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("should have stdout pipe");
    let parsed: Result<Items, _> = quick_xml::de::from_reader(BufReader::new(stdout));

    // Even when the parser throws an error, we first wait for the subprocess to exit.
    //
    // If the subprocess returned an error, then the parser probably tried to parse garbage output
    // so we will ignore the parser error and instead return the error printed by the subprocess.
    let result = child.wait_with_output()?;
    if !result.status.success() {
        let message = String::from_utf8_lossy(&result.stderr);

        return Err(Error::Command(format!(
            "nix-env failed with {}:\n{}",
            result.status, message,
        )));
    }

    Ok(parsed?.items)
}

/// A parser error that may occur during parsing `nix-env`'s output.
type ParserError = quick_xml::DeError;

#[derive(Debug, Deserialize)]
struct Output {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@path")]
    path: String,
}

#[derive(Debug, Deserialize)]
struct Item {
    #[serde(rename = "@attrPath")]
    attr_path: String,
    #[serde(rename = "@system")]
    system: String,
    #[serde(rename = "output", default)]
    outputs: Vec<Output>,
}

impl Item {
    fn consume<E: de::Error>(self, store_paths: &mut Vec<StorePath>) -> Result<(), E> {
        for output in self.outputs {
            let origin = PathOrigin {
                attr: self.attr_path.clone(),
                output: output.name,
                toplevel: true,
                system: Some(self.system.clone()),
            };
            let store_path = StorePath::parse(origin, &output.path).ok_or_else(|| {
                de::Error::custom(format!(
                    "store path does not match expected format /prefix/hash-name: {}",
                    output.path
                ))
            })?;
            store_paths.push(store_path);
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Items {
    #[serde(rename = "item", default, deserialize_with = "deserialize_items")]
    items: Vec<StorePath>,
}

fn deserialize_items<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<StorePath>, D::Error> {
    struct ItemsVisitor;

    impl<'de> Visitor<'de> for ItemsVisitor {
        type Value = Vec<StorePath>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "a sequence of `<item>` elements")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut store_paths = Vec::new();
            while let Some(item) = seq.next_element::<Item>()? {
                item.consume(&mut store_paths)?;
            }
            Ok(store_paths)
        }
    }

    deserializer.deserialize_seq(ItemsVisitor)
}

/// Enumeration of all the possible errors that may happen during querying the packages.
#[derive(Debug)]
pub enum Error {
    /// Parsing of the output failed
    Parse(ParserError),

    /// An IO error occurred
    Io(io::Error),

    /// nix-env failed with an error message
    Command(String),
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Parse(_) => "nix-env output parse error",
            Error::Io(_) => "io error",
            Error::Command(_) => "nix-env error",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        use self::Error::*;
        match *self {
            Parse(ref e) => write!(f, "parsing XML output of nix-env failed: {}", e),
            Io(ref e) => write!(f, "IO error: {}", e),
            Command(ref e) => write!(f, "nix-env failed with error: {}", e),
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        Error::Io(err)
    }
}

impl From<ParserError> for Error {
    fn from(err: ParserError) -> Error {
        Error::Parse(err)
    }
}
