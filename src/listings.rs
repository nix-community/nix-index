use std::fs::File;
use std::io;
use std::iter::FromIterator;

use futures::{Stream, StreamExt, TryFutureExt};
use indexmap::map::Entry;
use indexmap::IndexMap;
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use crate::errors::{Error, ErrorKind, Result, ResultExt};
use crate::files::FileTree;
use crate::hydra::Fetcher;
use crate::nixpkgs;
use crate::package::StorePath;
use crate::workset::{WorkSet, WorkSetHandle, WorkSetWatch};

// We also add some additional sets that only show up in `nix-env -qa -A someSet`.
//
// Some of these sets are not build directly by hydra. We still include them here
// since parts of these sets may be build as dependencies of other packages
// that are build by hydra. This way, our attribute path information is more
// accurate.
//
// We only need sets that are not marked "recurseIntoAttrs" here, since if they are,
// they are already part of normal_paths.
pub const EXTRA_SCOPES: [&str; 6] = [
    "xorg",
    "haskellPackages",
    "rPackages",
    "nodePackages",
    "coqPackages",
    "texlive.pkgs",
];

/// A stream of store paths (packages) with their associated file listings.
///
/// If a store path has no file listing (for example, because it is not built by hydra),
/// the file listing will be `None` instead.
pub trait FileListingStream: Stream<Item = Result<Option<(StorePath, String, FileTree)>>> {}
impl<T> FileListingStream for T where T: Stream<Item = Result<Option<(StorePath, String, FileTree)>>>
{}

/// Fetches all the file listings for the full closure of the given starting set of path.
///
/// This function will fetch the file listings of each path in the starting set. Additionally, it
/// will also determine the references of each path and recursively fetch the file listings for those
/// paths.
///
/// The `jobs` argument is used to specify how many requests should be done in parallel. No more than
/// `jobs` requests will be in-flight at any given time.
fn fetch_listings_impl(
    fetcher: &Fetcher,
    jobs: usize,
    starting_set: Vec<StorePath>,
) -> (impl FileListingStream + '_, WorkSetWatch) {
    // Create the queue that will hold all the paths that still need processing.
    // Initially, only the starting set needs processing.

    // We can't use FromIterator here as we want shorter paths to win
    let mut map: IndexMap<String, StorePath> = IndexMap::with_capacity(starting_set.len());

    for path in starting_set {
        let hash = path.hash().into();
        match map.entry(hash) {
            Entry::Occupied(mut e) => {
                if e.get().origin().attr.len() > path.origin().attr.len() {
                    e.insert(path);
                }
            }
            Entry::Vacant(e) => {
                e.insert(path);
            }
        };
    }

    let workset = WorkSet::from_queue(map);

    // Processes a single store path, fetching the file listing for it and
    // adding its references to the queue
    let process = move |mut handle: WorkSetHandle<_, _>, path: StorePath| async move {
        let Some(parsed) = fetcher
            .fetch_references(path.clone())
            .map_err(|e| Error::with_chain(e, ErrorKind::FetchReferences(path)))
            .await?
        else {
            return Ok(None);
        };

        for reference in parsed.references {
            let hash = reference.hash().into_owned();
            handle.add_work(hash, reference);
        }

        let path = parsed.store_path.clone();
        let nar_path = parsed.nar_path;

        match fetcher.fetch_files(&parsed.store_path).await {
            Err(e) => Err(Error::with_chain(e, ErrorKind::FetchFiles(path))),
            Ok(Some(files)) => Ok(Some((path, nar_path, files))),
            Ok(None) => Ok(None),
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
pub fn try_load_paths_cache() -> Result<Option<(impl FileListingStream, WorkSetWatch)>> {
    let file = match File::open("paths.cache") {
        Ok(file) => file,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).chain_err(|| ErrorKind::LoadPathsCache)?,
    };

    let mut input = io::BufReader::new(file);
    let fetched: Vec<(StorePath, String, FileTree)> =
        bincode::deserialize_from(&mut input).chain_err(|| ErrorKind::LoadPathsCache)?;
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

pub fn fetch_listings<'a>(
    fetcher: &'a Fetcher,
    jobs: usize,
    nixpkgs: &str,
    systems: Vec<Option<&str>>,
    show_trace: bool,
) -> Result<(impl FileListingStream + 'a, WorkSetWatch)> {
    let mut scopes = vec![None];
    scopes.extend(EXTRA_SCOPES.map(Some));

    let mut all_queries = vec![];
    for system in systems {
        for scope in &scopes {
            all_queries.push((system, scope));
        }
    }

    // Collect results in parallel.
    let all_paths = all_queries
        .par_iter()
        .flat_map_iter(|&(system, scope)| {
            nixpkgs::query_packages(nixpkgs, system, scope.as_deref(), show_trace).map(|x| {
                x.chain_err(|| ErrorKind::QueryPackages(scope.as_deref().map(|s| s.to_string())))
            })
        })
        .filter_map(|res| match res {
            Ok(path) => Some(path),
            Err(e) => {
                // Older versions of nixpkgs may not have all scopes, so we skip them instead
                // of completely bailing out.
                eprintln!("Error getting package set: {e}");
                None
            }
        })
        .collect::<Vec<_>>();

    Ok(fetch_listings_impl(fetcher, jobs, all_paths))
}
