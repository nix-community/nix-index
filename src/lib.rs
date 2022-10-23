#![cfg_attr(
    feature = "cargo-clippy",
    warn(
        filter_map,
        option_map_unwrap_or,
        option_map_unwrap_or_else,
        option_unwrap_used,
        stutter,
        wrong_pub_self_convention,
        print_stdout
    )
)]

pub mod database;
pub mod errors;
pub mod files;
pub mod frcode;
pub mod hydra;
pub mod listings;
pub mod nixpkgs;
pub mod package;
pub mod util;
pub mod workset;

/// The URL of the binary cache that we use to fetch file listings and references.
///
/// Hardcoded for now, but may be made a configurable option in the future.
pub const CACHE_URL: &str = "http://cache.nixos.org";
