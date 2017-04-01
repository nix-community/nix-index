//! A task queue where the processing of tasks can generate additional subtasks.
//!
//! This module implements a stream where the consumer of the stream can request
//! additional items to be added to the stream. An example where this is useful
//! is fetching a package including all the transitive dependencies: we start
//! with a stream that just yields the package we want to fetch. The consumer can
//! then fetch a package and add all dependencies of that package to the stream,
//! adding them to the set of packages that need to be fetched.
//!
//! The data structure is called a work set because it allows assigning a key to
//! each item to avoid duplicates. A new item will only be added if no prior item
//! had the same key.
//!
//! # Example
//!
//! ```rust
//! extern crate futures;
//! extern crate nix_index;
//!
//! use futures::{Stream};
//! use nix_index::workset::{WorkSet};
//! use std::iter;
//!
//! #[derive(Clone)]
//! struct Package {
//!     name: String,
//!     dependencies: Vec<Package>,
//! }
//!
//! fn main() {
//!     // set up some data
//!     let pkgA = Package { name: "a".to_string(), dependencies: vec![] };
//!     let pkgB = Package { name: "b".to_string(), dependencies: vec![] };
//!     let pkgC = Package { name: "c".to_string(), dependencies: vec![pkgA.clone(), pkgB] };
//!     let pkgD = Package { name: "d".to_string(), dependencies: vec![pkgA, pkgC] };
//!
//!     // construct a workset that has `pkgD` as initial item.
//!     let workset = WorkSet::from_iter(iter::once((pkgD.name.clone(), pkgD)));
//!
//!     // fetch the names of all transitive dependencies of `pkgD`. In real cases,
//!     // this would probably perform some network requests or other IO with futures.
//!     let all_packages = workset.map(|(mut handle, pkg)| {
//!         let Package { name, dependencies } = pkg;
//!         // add all dependencies to the workset
//!         for pkg in dependencies {
//!             handle.add_work(pkg.name.clone(), pkg);
//!         }
//!         name
//!     });
//!
//!    // all_packages is now a stream of all the names of the transitive dependencies of pkgD
//!    // and pkgD itself
//! }
//! ```
use futures::{Stream, Async, Poll};
use std::collections::HashSet;
use ordermap::OrderMap;
use void::Void;
use std::rc::{Rc, Weak};
use std::cell::RefCell;
use std::hash::Hash;
use std::iter::FromIterator;

/// This structure holds the internal state of our queue.
struct Shared<K, V> {
    /// The set of keys that have already been added to the queue sometime in the past.
    /// Any item whose key is in this set does not need to be added again.
    seen: HashSet<K>,

    /// The map of items that still need to be processed. As long as this is non-empty,
    /// there is still work remaining.
    queue: OrderMap<K, V>,
}

impl<K: Hash + Eq, V> Shared<K, V> {
    /// Add a task to the work queue if the given key still needs to be processed.
    /// Returns `true` if a new item was added, `false` otherwise.
    fn insert(&mut self, k: K, v: V) -> bool {
        use ordermap::Entry::*;
        if !self.seen.contains(&k) {
            match self.queue.entry(k) {
                Occupied(_) => return false,
                Vacant(e) => {
                    e.insert(v);
                    return true;
                }
            }
        }
        false
    }
}

/// A queue where the consumer can request new items to be added to the queue.
///
/// To construct a new instance of this type, use `WorkSet::from_iter`.
///
/// The queue terminates if there is no work left that need processing and all
/// `WorkSetHandle`s have been dropped (if there are `WorkSetHandle`s alive
/// then it is still possible to call `add_work`, so the stream cannot end even
/// if there is no work item available at the current time).
pub struct WorkSet<K, V> {
    /// A reference to the state of the queue.
    /// This reference is shared with all `WorkSetHandle`s.
    state: Rc<RefCell<Shared<K, V>>>,
}

/// A work set handle allows you to add new items to the queue.
///
/// As long as there are still `WorkSetHandle`s alive, the queue
/// will not terminate.
pub struct WorkSetHandle<K, V> {
    state: Rc<RefCell<Shared<K, V>>>,
}

impl<K: Hash + Eq, V> WorkSetHandle<K, V> {
    /// Adds a new item to the queue but only if this is
    /// the first time an item with the specified key is added.
    ///
    /// Returns `true` if this was a new item and therefore new work
    /// was added to the queue or `false` if there already was an item for
    /// the given key.
    pub fn add_work(&mut self, key: K, work: V) -> bool {
        self.state.borrow_mut().insert(key, work)
    }
}

/// An observer for `WorkSet` that provides status information
/// about the queue.
///
/// Note that this trait is not dependent on the type of items or keys
/// in the work set, as it only provides meta information about the queue.
pub trait WorkSetObserver {
    /// Returns the number of items in the queue that still need processing.
    fn queue_len(&self) -> usize;
}

/// A work set watch is any implementation of a `WorkSetObserver`.
///
/// The watch not prevent the queue from terminating. If the queue has already
/// terminated, the number of remaining items will be zero.
pub type WorkSetWatch = Box<WorkSetObserver>;

/// This is a concrete implementation of a `WorkSetObserver`.
///
/// The indirection through the `WorkSetObserver` trait and `WorkSetWatch` type is
/// necessary to allow hiding the concrete types `K` and `V` of the queue.
/// Hiding the concrete types makes the interface much nicer.
#[derive(Clone)]
struct WorkSetObserverImpl<K, V> {
    /// A weak reference to the queue state. The reference is weak
    /// so that the the observer does not prevent the queue from terminating.
    state: Weak<RefCell<Shared<K, V>>>,
}

impl<K, V> WorkSetObserver for WorkSetObserverImpl<K, V> {
    fn queue_len(&self) -> usize {
        self.state
            .upgrade()
            .map_or(0,
                    |shared: Rc<RefCell<Shared<K, V>>>| shared.as_ref().borrow().queue.len())
    }
}


impl<K: Hash + Eq + 'static, V: 'static> WorkSet<K, V> {
    /// Returns a watch for this work set that provides status information.
    pub fn watch(&self) -> WorkSetWatch {
        Box::new(WorkSetObserverImpl { state: Rc::downgrade(&self.state) })
    }
}

/// Constructs a new work set with the given initial work items.
impl<K: Hash + Eq + 'static, V: 'static> FromIterator<(K, V)> for WorkSet<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> WorkSet<K, V> {
        let shared = Shared {
            seen: HashSet::new(),
            queue: OrderMap::from_iter(iter),
        };
        WorkSet { state: Rc::new(RefCell::new(shared)) }
    }
}

/// A work set implements the `Stream` trait. The stream will produce the work
/// that still needs processing. Along with every work item it also provides
/// a handle to the queue that allows the consumer to add more items to the queue.
///
/// The stream ends if the queue terminates, see the documentation of `WorkSet`
/// for when exactly that happens.
impl<K: Hash + Eq, V> Stream for WorkSet<K, V> {
    type Item = (WorkSetHandle<K, V>, V);
    type Error = Void;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let (k, v) = match self.state.borrow_mut().queue.pop() {
            Some(e) => e,
            None => {
                return Ok({
                              if Rc::strong_count(&self.state) == 1 {
                                  Async::Ready(None)
                              } else {
                                  Async::NotReady
                              }
                          })
            }
        };

        self.state.borrow_mut().seen.insert(k);
        let handle = WorkSetHandle { state: self.state.clone() };
        Ok(Async::Ready(Some((handle, v))))
    }
}
