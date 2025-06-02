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

pub fn cache_dir() -> &'static std::ffi::OsStr {
    let base = xdg::BaseDirectories::with_prefix("nix-index").unwrap();
    let cache_dir = Box::new(base.get_cache_home());
    let cache_dir = Box::leak(cache_dir);
    cache_dir.as_os_str()
}

/// The URL of the binary cache that we use to fetch file listings and references.
///
/// Hardcoded for now, but may be made a configurable option in the future.
pub const CACHE_URL: &str = "https://cache.nixos.org";
