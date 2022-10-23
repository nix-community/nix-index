use std::fs::File;
use std::io;
use std::iter::FromIterator;
use std::pin::Pin;

use futures::{future, FutureExt, Stream, StreamExt, TryFutureExt};

use crate::errors::{Error, ErrorKind, Result, ResultExt};
use crate::files::FileTree;
use crate::hydra::Fetcher;
use crate::nixpkgs;
use crate::package::StorePath;
use crate::workset::{WorkSet, WorkSetHandle, WorkSetWatch};

/// A stream of store paths (packages) with their associated file listings.
///
/// If a store path has no file listing (for example, because it is not built by hydra),
/// the file listing will be `None` instead.
pub type FileListingStream<'a> =
    Pin<Box<dyn Stream<Item = Result<Option<(StorePath, String, FileTree)>>> + 'a>>;

/// Fetches all the file listings for the full closure of the given starting set of path.
///
/// This function will fetch the file listings of each path in the starting set. Additionally, it
/// will also determine the references of each path and recursively fetch the file listings for those
/// paths.
///
/// The `jobs` argument is used to specify how many requests should be done in parallel. No more than
/// `jobs` requests will be in-flight at any given time.
pub fn fetch_file_listings(
    fetcher: &Fetcher,
    jobs: usize,
    starting_set: Vec<StorePath>,
) -> (FileListingStream, WorkSetWatch) {
    // Create the queue that will hold all the paths that still need processing.
    // Initially, only the starting set needs processing.
    let workset = WorkSet::from_iter(starting_set.into_iter().map(|x| (x.hash().into_owned(), x)));

    // Processes a single store path, fetching the file listing for it and
    // adding its references to the queue
    let process = move |mut handle: WorkSetHandle<_, _>, path: StorePath| {
        fetcher
            .fetch_references(path.clone())
            .map_err(|e| Error::with_chain(e, ErrorKind::FetchReferences(path)))
            .and_then(move |parsed| match parsed {
                Some(parsed) => {
                    for reference in parsed.references {
                        let hash = reference.hash().into_owned();
                        handle.add_work(hash, reference);
                    }

                    let path = parsed.store_path;
                    let nar_path = parsed.nar_path;
                    future::Either::Left(fetcher.fetch_files(&path).map(move |r| match r {
                        Err(e) => Err(Error::with_chain(e, ErrorKind::FetchFiles(path))),
                        Ok(Some(files)) => Ok(Some((path, nar_path, files))),
                        Ok(None) => Ok(None),
                    }))
                }
                None => future::Either::Right(future::ok(None)),
            })
    };

    // Process all paths in the queue, until the queue becomes empty.
    let watch = workset.watch();
    let stream = workset
        .map(move |(handle, path)| process(handle, path))
        .buffer_unordered(jobs);
    (Box::pin(stream), watch)
}

/// Tries to load the file listings for all paths from a cache file named `paths.cache`.
///
/// This function is used to implement the `--path-cache` option.
pub fn try_load_paths_cache() -> Result<Option<(FileListingStream<'static>, WorkSetWatch)>> {
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

    Ok(Some((Box::pin(stream), watch)))
}
