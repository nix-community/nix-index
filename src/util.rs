use std::io::{self, Write};
use std::path::{PathBuf};
use std::fs::{OpenOptions};
use std::env;

use futures::{Future, IntoFuture, Poll};
use futures::future::{self, Either, FutureResult};

pub fn write_temp_file(base_name: &str, contents: &[u8]) -> Option<PathBuf> {
    let mut path = None;
    for i in 0.. {
        let mut this_path = env::temp_dir();
        if i == 0 {
            this_path.push(base_name);
        } else {
            this_path.push(format!("{}.{}", base_name, i));
        }
        let temp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&this_path);
        match temp_file {
            Ok(mut file) => {
                path = file.write_all(contents).map(|_| { this_path }).ok();
                break
            },
            Err(e) => {
                if e.kind() != io::ErrorKind::AlreadyExists {
                    break
                }
            }
        }
    }
    path
}

pub struct Retry<F, A: IntoFuture> {
    f: F,
    current_future: A::Future,
    tries_left: isize
}

impl<F, A> Future for Retry<F, A>
    where F: FnMut() -> A,
          A: IntoFuture,
{
    type Error = A::Error;
    type Item = A::Item;

    fn poll(&mut self) -> Poll<A::Item, A::Error> {
        loop {
            self.current_future = match self.current_future.poll() {
                Ok(v) => { return Ok(v) }
                Err(e) => {
                    self.tries_left -= 1;

                    if self.tries_left <= 0 {
                        return Err(e)
                    } else {
                        (self.f)().into_future()
                    }
                },
            }
        }
    }

}

pub fn retry<F, A>(max_tries: isize, mut f: F) -> Retry<F, A>
    where F: FnMut() -> A,
          A: IntoFuture,
{
    Retry {
        current_future: f().into_future(),
        f: f,
        tries_left: max_tries
    }
}

pub fn future_result<F, A>(f: F) -> Either<A::Future, FutureResult<A::Item, A::Error>>
    where A: IntoFuture,
          F: FnOnce() -> Result<A, A::Error>,
{
    match f() {
        Ok(v) => Either::A(v.into_future()),
        Err(e) => Either::B(future::err(e)),
    }
}
