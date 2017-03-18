use futures::{Stream, Async, Poll};
use std::collections::{HashSet};
use ordermap::{OrderMap};
use void::{Void};
use std::rc::{Rc, Weak};
use std::cell::{RefCell};
use std::hash::{Hash};
use std::iter::{FromIterator};

struct Shared<K, V> {
    seen: HashSet<K>,
    queue: OrderMap<K, V> ,
}

impl<K: Hash + Eq, V> Shared<K, V> {
    fn insert(&mut self, k: K, v: V) -> bool {
        use ordermap::Entry::*;
        if !self.seen.contains(&k) {
            match self.queue.entry(k) {
                Occupied(_) => return false,
                Vacant(e) => {
                    e.insert(v);
                    return true
                }
            }
        }
        false
    }
}

pub struct WorkSet<K, V> {
    state: Rc<RefCell<Shared<K, V>>>,
}

pub struct WorkSetHandle<K, V> {
    state: Rc<RefCell<Shared<K, V>>>,
}

impl<K: Hash + Eq, V> WorkSetHandle<K, V> {
    pub fn add_work(&mut self, key: K, work: V) -> bool {
        self.state.borrow_mut().insert(key, work)
    }

}

pub trait WorkSetObserver {
    fn queue_len(&self) -> usize;
}

pub type WorkSetWatch = Box<WorkSetObserver>;

#[derive(Clone)]
struct WorkSetObserverImpl<K, V> {
    state: Weak<RefCell<Shared<K, V>>>,
}

impl<K, V> WorkSetObserver for WorkSetObserverImpl<K, V> {
    fn queue_len(&self) -> usize {
        self.state.upgrade().map_or(0, |shared: Rc<RefCell<Shared<K, V>>>| shared.as_ref().borrow().queue.len())
    }
}


impl<K: Hash + Eq + 'static, V: 'static> WorkSet<K, V> {
    pub fn from_iter<I: Iterator<Item=(K,V)>>(iter: I) -> WorkSet<K, V> {
        let shared = Shared {
            seen: HashSet::new(),
            queue: OrderMap::from_iter(iter),
        };
        WorkSet {
            state: Rc::new(RefCell::new(shared)),
        }
    }

    pub fn watch(&self) -> WorkSetWatch {
        Box::new(WorkSetObserverImpl {
            state: Rc::downgrade(&self.state),
        })
    }
}

impl<K: Hash + Eq, V> Stream for WorkSet<K, V> {
    type Item = (WorkSetHandle<K, V>, V);
    type Error = Void;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let (k,v) = match self.state.borrow_mut().queue.pop() {
            Some(e) => e,
            None => return Ok({
                if Rc::strong_count(&self.state) == 1 {
                    Async::Ready(None)
                } else {
                    Async::NotReady
                }
            })
        };

        self.state.borrow_mut().seen.insert(k);
        let handle = WorkSetHandle { state: self.state.clone() };
        Ok(Async::Ready(Some((handle, v))))
    }
}
