#![warn(
    clippy::manual_filter_map,
    clippy::map_unwrap_or,
    clippy::module_name_repetitions,
    clippy::print_stdout,
    clippy::unwrap_used
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
