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
pub mod files;
pub mod frcode;
pub mod hydra;
pub mod nixpkgs;
pub mod package;
pub mod util;
pub mod workset;
