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
/// If `main_program` is true, nix-env is also passed `--meta` so that each package's
/// `meta.mainProgram` is included in the output.
///
/// The function returns an [`IntoIterator`] over the packages returned by nix-env.
pub fn query_packages(
    nixpkgs: &str,
    system: Option<&str>,
    scope: Option<&str>,
    show_trace: bool,
    main_program: bool,
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

    if main_program {
        // Query package meta so we can read `meta.mainProgram`, used to synthesize a
        // `/bin/$mainProgram` listing for packages not built by Hydra. We skip it otherwise:
        // `--meta` makes nix-env emit roughly ten times as much output.
        cmd.arg("--meta");
    }

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

#[derive(Debug)]
pub struct PackageOutput {
    pub path: StorePath,
    pub main_program: Option<String>,
}

/// An [`IntoIterator`] of parsed store paths and main programs from the output of nix-env.
///
/// Use `query_packages` to create a value of this type.
pub type Packages = Vec<PackageOutput>;

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

    Ok(parsed?.outputs)
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
struct Meta {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@value")]
    value: Option<String>,
}

fn main_program<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Vec::<Meta>::deserialize(deserializer)?
        .into_iter()
        .find(|meta| meta.name == "mainProgram")
        .and_then(|meta| meta.value))
}

#[derive(Debug, Deserialize)]
struct Item {
    #[serde(rename = "@attrPath")]
    attr_path: String,
    #[serde(rename = "@outputName", default)]
    output_name: String,
    #[serde(rename = "@system")]
    system: String,
    #[serde(rename = "output", default)]
    outputs: Vec<Output>,
    #[serde(rename = "meta", default, deserialize_with = "main_program")]
    main_program: Option<String>,
}

impl Item {
    fn consume<E: de::Error>(self, outputs: &mut Vec<PackageOutput>) -> Result<(), E> {
        // By convention (`lib.getExe`), `meta.mainProgram` names the executable
        // `bin/$mainProgram` in the package's "bin" output, falling back to its "out"
        // output and then its default output (the `lib.getBin` chain `pkg.bin or pkg.out or pkg`).
        // It is not in every output, so tag only that one.
        let main_output: &str = if self.outputs.iter().any(|output| output.name == "bin") {
            "bin"
        } else if self.outputs.iter().any(|output| output.name == "out") {
            "out"
        } else {
            &self.output_name
        };

        for output in self.outputs {
            let main_program = if output.name == main_output {
                self.main_program.clone()
            } else {
                None
            };
            let origin = PathOrigin {
                attr: self.attr_path.clone(),
                output: output.name,
                toplevel: true,
                system: Some(self.system.clone()),
            };
            let path = StorePath::parse(origin, &output.path).ok_or_else(|| {
                de::Error::custom(format!(
                    "store path does not match expected format /prefix/hash-name: {}",
                    output.path
                ))
            })?;
            outputs.push(PackageOutput { path, main_program });
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Items {
    #[serde(rename = "item", default, deserialize_with = "package_outputs")]
    outputs: Vec<PackageOutput>,
}

fn package_outputs<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<PackageOutput>, D::Error> {
    struct ItemsVisitor;

    impl<'de> Visitor<'de> for ItemsVisitor {
        type Value = Vec<PackageOutput>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "a sequence of `<item>` elements")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut outputs = Vec::new();
            while let Some(item) = seq.next_element::<Item>()? {
                item.consume(&mut outputs)?;
            }
            Ok(outputs)
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
