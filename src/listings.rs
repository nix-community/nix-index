use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::iter::FromIterator;

use futures::{Stream, StreamExt};
use indexmap::map::Entry;
use indexmap::IndexMap;
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use serde_bytes::ByteBuf;

use crate::errors::{Error, Result};
use crate::files::FileTree;
use crate::hydra::Fetcher;
use crate::nixpkgs::{self, PackageOutput};
use crate::package::StorePath;
use crate::workset::{WorkSet, WorkSetHandle, WorkSetWatch};

/// A stream of store paths (packages) with their associated file listings.
///
/// If a store path has no file listing (for example, because it is not built by hydra),
/// the file listing will be `None` instead.
pub trait FileListingStream: Stream<Item = Result<Option<(StorePath, String, FileTree)>>> {}
impl<T> FileListingStream for T where T: Stream<Item = Result<Option<(StorePath, String, FileTree)>>>
{}

/// Builds a synthetic file listing containing just `/bin/$main_program`.
///
/// This can be used to supplement the set of packages built by Hydra. Note that this function
/// always returns zero for the size of the executable file, and an empty string for the nar.
fn synthesize_main_program(path: StorePath, main_program: String) -> (StorePath, String, FileTree) {
    let tree = HashMap::from([(
        ByteBuf::from(b"bin".to_vec()),
        FileTree::directory(HashMap::from([(
            ByteBuf::from(main_program.into_bytes()),
            FileTree::regular(0, true),
        )])),
    )]);
    (path, String::new(), FileTree::directory(tree))
}

/// Fetches all the file listings for the full closure of the given starting set of path.
///
/// This function will fetch the file listings of each path in the starting set. Additionally, it
/// will also determine the references of each path and recursively fetch the file listings for those
/// paths.
///
/// The `jobs` argument is used to specify how many requests should be done in parallel. No more than
/// `jobs` requests will be in-flight at any given time.
#[allow(clippy::result_large_err)]
fn fetch_listings_impl(
    fetcher: &Fetcher,
    jobs: usize,
    starting_set: Vec<PackageOutput>,
) -> (impl FileListingStream + '_, WorkSetWatch) {
    // Create the queue that will hold all the paths that still need processing.
    // Initially, only the starting set needs processing.

    // We can't use FromIterator here as we want shorter paths to win
    let mut map: IndexMap<String, PackageOutput> = IndexMap::with_capacity(starting_set.len());

    for output in starting_set {
        let hash = output.path.hash().into();
        match map.entry(hash) {
            Entry::Occupied(mut e) => {
                if e.get().path.origin().attr.len() > output.path.origin().attr.len() {
                    e.insert(output);
                }
            }
            Entry::Vacant(e) => {
                e.insert(output);
            }
        };
    }

    let workset = WorkSet::from_queue(map);

    // Processes a single store path, fetching the file listing for it and
    // adding its references to the queue
    let process = move |mut handle: WorkSetHandle<_, _>, PackageOutput { path, main_program }| async move {
        let parsed = match fetcher.fetch_references(path.clone()).await {
            Err(e) => return Err(Error::FetchReferences { path, source: e }),
            Ok(Some(parsed)) => parsed,
            Ok(None) => return Ok(main_program.map(|p| synthesize_main_program(path, p))),
        };

        for reference in parsed.references {
            let hash = reference.hash().into_owned();
            let output = PackageOutput {
                path: reference,
                main_program: None,
            };
            handle.add_work(hash, output);
        }

        let path = parsed.store_path.clone();
        let nar_path = parsed.nar_path;

        match fetcher.fetch_files(&parsed.store_path).await {
            Err(e) => Err(Error::FetchFiles {
                path: parsed.store_path,
                source: e,
            }),
            Ok(Some(files)) => Ok(Some((path, nar_path, files))),
            Ok(None) => Ok(main_program.map(|p| synthesize_main_program(parsed.store_path, p))),
        }
    };

    // Process all paths in the queue, until the queue becomes empty.
    let watch = workset.watch();
    let stream = workset
        .map(move |(handle, path)| process(handle, path))
        .buffer_unordered(jobs);
    (stream, watch)
}

/// Tries to load the file listings for all paths from a cache file named `paths.cache`.
///
/// This function is used to implement the `--path-cache` option.
#[allow(clippy::result_large_err)]
pub fn try_load_paths_cache() -> Result<Option<(impl FileListingStream, WorkSetWatch)>> {
    let file = match File::open("paths.cache") {
        Ok(file) => file,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::LoadPathsCache { source: e })?,
    };

    let mut input = io::BufReader::new(file);
    let fetched: Vec<(StorePath, String, FileTree)> =
        bincode::serde::decode_from_std_read(&mut input, bincode::config::standard())
            .map_err(|e| Error::ParsePathsCache { source: e })?;
    let workset = WorkSet::from_iter(
        fetched
            .into_iter()
            .map(|(path, nar, tree)| (path.hash().to_string(), Some((path, nar, tree)))),
    );
    let watch = workset.watch();
    let stream = workset.map(|r| {
        let (_handle, v) = r;
        Ok(v)
    });

    Ok(Some((stream, watch)))
}

#[allow(clippy::result_large_err)]
pub fn fetch<'a>(
    fetcher: &'a Fetcher,
    jobs: usize,
    nixpkgs: &str,
    systems: Vec<Option<&str>>,
    extra_scopes: &[String],
    show_trace: bool,
    main_program: bool,
) -> Result<(impl FileListingStream + 'a, WorkSetWatch)> {
    let mut scopes = vec![None];
    scopes.extend(
        extra_scopes
            .iter()
            // allow --extra-scopes ""
            .filter(|x| !x.is_empty())
            .cloned()
            .map(Some),
    );

    let mut all_queries = vec![];
    for system in systems {
        for scope in &scopes {
            all_queries.push((system, scope));
        }
    }

    // Collect results in parallel.
    let all_paths: nixpkgs::Packages = all_queries
        .par_iter()
        .map(|&(system, scope)| {
            nixpkgs::query_packages(nixpkgs, system, scope.as_deref(), show_trace, main_program)
        })
        .collect::<std::result::Result<Vec<nixpkgs::Packages>, nixpkgs::Error>>()
        .map_err(|e| Error::QueryPackages { source: e })?
        .into_iter()
        .flatten()
        .collect();

    Ok(fetch_listings_impl(fetcher, jobs, all_paths))
}
