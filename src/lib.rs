#![cfg_attr(
    feature = "cargo-clippy",
    warn(
        clippy::manual_filter_map,
        clippy::map_unwrap_or,
        clippy::module_name_repetitions,
        clippy::print_stdout,
        clippy::unwrap_used,
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
pub const CACHE_URL: &str = "https://cache.nixos.org";
