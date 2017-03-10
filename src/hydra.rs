use curl;
use serde;
use serde_json;
use super::{util};

use std::fmt;
use std::str::{self, Utf8Error};
use std::collections::HashMap;
use std::io::{self, Write, Read};
use std::sync::{Arc, Mutex};
use std::path::{PathBuf};
use std::time::{Instant, Duration};
use tokio_curl::Session;
use curl::easy::Easy;
use futures::{Future};
use futures::future;
use xz2::read::XzDecoder;
use serde::de::{Deserialize, Deserializer, MapVisitor, Visitor};
use serde::bytes::{ByteBuf};

use files::{Files, File};
use nixpkgs::{StorePath};

#[derive(Deserialize, Debug, PartialEq)]
struct FileListingResponse {
    root: HydraFileListing
}

#[derive(Debug, PartialEq)]
struct HydraFileListing(Files);

pub fn fetch_files<'a>(cache_url: &str, session: &'a Session, path: &StorePath) ->
    Box<Future<Item=Option<Files>, Error=Error> + 'a>
{
    let url = format!("{}/{}.ls.xz", cache_url, path.hash());
    let name = format!("{}.json", path.hash());

    util::future_result(|| {
        let mut req = Easy::new();

        req.url(&url)?;

        let buffer = Arc::new(Mutex::new(Vec::new()));

        let sink = buffer.clone();
        req.write_function(move |data| {
            sink.lock().unwrap().write(data).unwrap();
            Ok(data.len())
        })?;

        let process_response = move |mut res: Easy| {
            let code = res.response_code()?;

            if code == 404 {
                return Ok(None)
            }

            if code / 100 != 2 {
                return Err(Error::Http(url, code))
            }

            let data = buffer.lock().unwrap();
            let mut reader = XzDecoder::new(io::Cursor::new(&*data));
            let mut contents = Vec::new();
            reader.read_to_end(&mut contents)?;

            let now = Instant::now();
            let response: FileListingResponse = serde_json::from_slice(&contents).map_err(|e| {
                Error::Parse(url, e,  util::write_temp_file("file_listing.json", &contents))
            })?;
            let duration = now.elapsed();

            if duration > Duration::from_millis(2000) {
                let secs = duration.as_secs();
                let millis = duration.subsec_nanos() / 1000000;

                writeln!(&mut io::stderr(), "warning: took a long time to parse: {}s:{:03}ms", secs, millis)?;
                if let Some(p) = util::write_temp_file(&name, &contents) {
                    writeln!(&mut io::stderr(), "saved response to file: {}", p.to_string_lossy())?;
                }
            }

            Ok(Some(response.root.0))
        };

        Ok(session.perform(req).map_err(|e| Error::Io(e.into_error())).and_then(move |res| {
            future::result(process_response(res))
        }))
    }).boxed()
}

impl Deserialize for HydraFileListing {
    fn deserialize<D: Deserializer>(d: D) -> Result<HydraFileListing, D::Error> {
        struct Root;

        impl Visitor for Root {
            type Value = Files;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a file listing (map)")
            }

            fn visit_map<V: MapVisitor>(self, mut visitor: V) -> Result<Files, V::Error> {
                const VARIANTS: &'static [&'static str] = &["regular", "directory", "symlink"];

                let mut typ: Option<ByteBuf> = None;
                let mut size: Option<u64> = None;
                let mut executable: Option<bool> = None;
                let mut entries: Option<HashMap<ByteBuf, HydraFileListing>> = None;
                let mut target: Option<ByteBuf> = None;

                while let Some(key) = try!(visitor.visit_key::<ByteBuf>()) {
                    match &key as &[u8] {
                        b"type" => {
                            if typ.is_some() {
                                return Err(serde::de::Error::duplicate_field("type"));
                            }
                            typ = Some(try!(visitor.visit_value()))
                        },
                        b"size" => {
                            if size.is_some() {
                                return Err(serde::de::Error::duplicate_field("size"));
                            }
                            size = Some(try!(visitor.visit_value()))
                        },
                        b"executable" => {
                            if executable.is_some() {
                                return Err(serde::de::Error::duplicate_field("executable"));
                            }
                            executable = Some(try!(visitor.visit_value()))
                        },
                        b"entries" => {
                            if entries.is_some() {
                                return Err(serde::de::Error::duplicate_field("entries"));
                            }
                            entries = Some(try!(visitor.visit_value()))
                        },
                        b"target" => {
                            if target.is_some() {
                                return Err(serde::de::Error::duplicate_field("target"));
                            }
                            target = Some(try!(visitor.visit_value()))
                        },
                        _ => { try!(visitor.visit_value::<serde::de::impls::IgnoredAny>()); }
                    }
                }

                let typ = &try!(typ.ok_or(serde::de::Error::missing_field("type"))) as &[u8];

                match typ {
                    b"regular" => {
                        let size = size.ok_or(serde::de::Error::missing_field("size"))?;
                        let executable = executable.unwrap_or(false);
                        Ok(Files::Leaf(File::Regular { size: size, executable: executable }))
                    },
                    b"directory" => {
                        let entries = entries.ok_or(serde::de::Error::missing_field("entries"))?;
                        let entries = entries.into_iter().map(|(k, v)| (k, v.0)).collect();
                        Ok(Files::Directory { entries: entries })
                    },
                    b"symlink" => {
                        let target = target.ok_or(serde::de::Error::missing_field("target"))?;
                        Ok(Files::Leaf(File::Symlink { target: target }))
                    },
                    _ => {
                        Err(serde::de::Error::unknown_variant(&String::from_utf8_lossy(typ), VARIANTS))
                    }
                }
            }
        }
        d.deserialize_map(Root).map(|f| HydraFileListing(f) )
    }
}

pub enum Error {
    Io(io::Error),
    Curl(curl::Error),
    Http(String, u32),
    Parse(String, serde_json::Error, Option<PathBuf>),
    InvalidStorePath(String),
    InvalidUnicode(Vec<u8>, Utf8Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self { Error::Io(err) }
}

impl From<curl::Error> for Error {
    fn from(err: curl::Error) -> Self { Error::Curl(err) }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error>{
        use self::Error::*;
        match self {
            &Io(ref e) => write!(f, "input/output error: {}", e),
            &Curl(ref e) => write!(f, "curl error: {}", e),
            &Http(ref url, c) => write!(f, "error while fetching {}: http code {}", url, c),
            &Parse(ref url, ref e, ref contents) => {
                write!(f, "failed to parse file listing {}: {}", url, e)?;
                if let &Some(ref p) = contents {
                    write!(f, "\nresponse written to {}", p.to_string_lossy())?
                }
                Ok(())
            },
            &InvalidStorePath(ref s) => {
                write!(f, "failed to parse store path, must match format $(NIX_STORE_DIR)$(HASH)-name: {}", s)
            },
            &InvalidUnicode(ref bytes, ref e) => {
                write!(f, "invalid unicode byte {}: {}", bytes[e.valid_up_to()], e)
            }
        }
    }
}

pub fn fetch_references<'a>(cache_url: &str, session: &'a Session, mut path: StorePath) ->
    Box<Future<Item=(StorePath, Vec<StorePath>), Error=Error> + 'a>
{
    let url = format!("{}/{}.narinfo", cache_url, path.hash());

    util::future_result(|| {
        let mut req = Easy::new();

        req.url(&url)?;

        let buffer = Arc::new(Mutex::new(Vec::new()));

        let sink = buffer.clone();
        req.write_function(move |data| {
            sink.lock().unwrap().write(data).unwrap();
            Ok(data.len())
        })?;

        let process_response = move |mut res: Easy| {
            let code = res.response_code()?;

            if code == 404 {
                return Ok((path, Vec::new()))
            }

            if code / 100 != 2 {
                return Err(Error::Http(url, code))
            }

            let data = buffer.lock().unwrap();

            let references = b"References:";
            let store_path = b"StorePath:";
            let mut result = Vec::new();
            for line in data.split(|x| x == &b'\n') {
                if line.starts_with(references) {
                    let line = &line[references.len()..];
                    let line = str::from_utf8(line).map_err(|e| Error::InvalidUnicode(line.to_vec(), e))?;
                    result = line.trim().split_whitespace().map(|path| {
                        StorePath::parse(path).ok_or(Error::InvalidStorePath(path.to_string()))
                    }).collect::<Result<Vec<_>, _>>()?;
                }

                if line.starts_with(store_path) {
                    let line = &line[references.len()..];
                    let line = str::from_utf8(line).map_err(|e| Error::InvalidUnicode(line.to_vec(), e))?;
                    let line = line.trim();

                    path = StorePath::parse(line).ok_or(Error::InvalidStorePath(line.to_string()))?;
                }
            }

            Ok((path, result))
        };

        Ok(session.perform(req).map_err(|e| Error::Io(e.into_error())).and_then(move |res| {
            future::result(process_response(res))
        }))
    }).boxed()
}
