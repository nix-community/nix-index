use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{hydra, nixpkgs, package::StorePath};

#[derive(Error, Debug)]
pub enum Error {
    #[error("querying available packages failed: {source}")]
    QueryPackages {
        #[source]
        source: nixpkgs::Error,
    },
    #[error("fetching the file listing for store path '{path}' failed: {source}")]
    FetchFiles {
        path: StorePath,
        #[source]
        source: hydra::Error,
    },
    #[error("fetching the references of store path '{path}' failed: {source}")]
    FetchReferences {
        path: StorePath,
        #[source]
        source: hydra::Error,
    },
    #[error("reading the paths.cache file failed: {source}")]
    LoadPathsCache {
        #[source]
        source: io::Error,
    },
    #[error("parsing the paths.cache file failed: {source}")]
    ParsePathsCache {
        #[source]
        source: bincode::error::DecodeError,
    },
    #[error("writing the paths.cache file failed: {source}")]
    WritePathsCache {
        #[source]
        source: Box<dyn std::error::Error>,
    },
    #[error("creating the database at '{path:?}' failed: {source}")]
    CreateDatabase {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error>,
    },
    #[error("creating the directory for the database at '{path:?}' failed: {source}")]
    CreateDatabaseDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("writing to the database '{path:?}' failed: {source}")]
    WriteDatabase {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Can not parse proxy settings: {0}")]
    ParseProxy(#[from] crate::hydra::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
