#![cfg_attr(feature = "cargo-clippy", warn(filter_map, option_map_unwrap_or, option_map_unwrap_or_else, option_unwrap_used, stutter, wrong_pub_self_convention, print_stdout))]

#[macro_use]
extern crate serde_derive;
extern crate ansi_term;
extern crate bincode;
extern crate futures;
extern crate grep;
extern crate ordermap;
extern crate rustc_serialize;
extern crate serde;
extern crate serde_json;
extern crate tokio_core;
extern crate tokio_timer;
extern crate tokio_retry;
extern crate void;
extern crate xml;
extern crate zstd;
extern crate memchr;
extern crate byteorder;
extern crate xz2;
extern crate hyper;
#[macro_use]
extern crate error_chain;
extern crate brotli2;

pub mod nixpkgs;
pub mod files;
pub mod hydra;
pub mod util;
pub mod workset;
pub mod frcode;
pub mod package;
pub mod database;
