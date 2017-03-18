use std::io::{self, Write};
use std::path::{PathBuf};
use std::fs::{OpenOptions};
use std::env;

use futures::{IntoFuture};
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

pub fn future_result<F, A>(f: F) -> Either<A::Future, FutureResult<A::Item, A::Error>>
    where A: IntoFuture,
          F: FnOnce() -> Result<A, A::Error>,
{
    match f() {
        Ok(v) => Either::A(v.into_future()),
        Err(e) => Either::B(future::err(e)),
    }
}
