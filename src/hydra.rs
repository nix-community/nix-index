//! Interacting with hydra and the binary cache.
//!
//! This module has all functions that deal with accessing hydra or the binary cache.
//! Currently, it only provides two functions: `fetch_files` to get the file listing for
//! a store path and `fetch_references` to retrieve the references from the narinfo.
use serde;
use serde_json;
use super::util;

use std::fmt;
use std::result;
use std::str::{self, Utf8Error, FromStr};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Instant, Duration};
use futures::{Stream, Future};
use futures::future::{self, Either};
use xz2::write::XzDecoder;
use serde::de::{Deserialize, Deserializer, MapVisitor, Visitor};
use serde::bytes::ByteBuf;
use hyper::client::{Client, Response, Connect};
use hyper::{self, Uri, StatusCode};
use hyper::header::{ContentEncoding, Encoding, Headers};
use brotli2::write::BrotliDecoder;

use files::FileTree;
use package::{StorePath, PathOrigin};

error_chain! {
    errors {
        Http(url: String, code: StatusCode) {
            description("http status code error")
            display("request GET '{}' failed with HTTP error {}", url, code)
        }
        ParseResponse(url: String, tmp_file: Option<PathBuf>) {
            description("response parse error")
            display("response to GET '{}' failed to parse{}", url, tmp_file.as_ref().map_or("".into(), |f| format!(" (response saved to {})", f.to_string_lossy())))
        }
        ParseStorePath(url: String, path: String) {
            description("store path parse error")
            display("response to GET '{}' contained invalid store path '{}', expected string matching format $(NIX_STORE_DIR)$(HASH)-$(NAME)", url, path)
        }
        Unicode(url: String, bytes: Vec<u8>, err: Utf8Error) {
            description("unicode error")
            display("response to GET '{}' contained invalid unicode byte {}: {}", url, bytes[err.valid_up_to()], err)
        }
        Decode(url: String) {
            description("decoder error")
            display("response to GET '{}' could not be decoded", url)
        }
        UnsupportedEncoding(url: String, encoding: Option<ContentEncoding>) {
            description("unsupported content-encoding")
            display(
                "response to GET '{}' had unsupported content-encoding ({})",
                url,
                encoding.as_ref().map_or("not present".to_string(), |v| format!("'{}'", v)),
            )
        }
    }
    foreign_links {
        Hyper(hyper::Error);
    }
}

/// This enum lists the compression algorithms that we support for responses from hydra.
enum SupportedEncoding {
    /// File listings used to be xz encoded, so we have to support this.
    /// Nar's themselves still use the xz compression.
    Xz,

    /// The new format for file lisitings uses brotli compression.
    Brotli,

    /// This indicates that there is no compression at all, for example
    /// used for `.narinfo`s.
    Identity,
}

/// Reads the encoding of the response from the request headers.
///
/// If the request headers indicate an unsupported encoding, this function returns `None`.
///
/// If there is no `Content-Encoding` header we assume that the content is encoded with
/// the `Identity` variant (i.e. there is no compression at all).
fn compute_encoding(headers: &Headers) -> Option<SupportedEncoding> {
    let empty = ContentEncoding(vec![]);
    let &ContentEncoding(ref encodings) = headers.get::<ContentEncoding>().unwrap_or(&empty);

    let identity = Encoding::Identity;
    let encoding = encodings.get(0).unwrap_or(&identity);
    match *encoding {
        Encoding::Brotli => Some(SupportedEncoding::Brotli),
        Encoding::Identity => Some(SupportedEncoding::Identity),
        Encoding::EncodingExt(ref ext) if ext == "xz" => Some(SupportedEncoding::Xz),
        _ => None,
    }
}

/// Sends a GET request to the given URL and decodes the response with the given encoding.
///
/// If `encoding` is `None`, then the encoding will be detected automatically by reading
/// the `Content-Encoding` header.
///
/// The returned future resolves to `(url, None)` if the server returned a 404 error. On any
/// other error, the future resolves to an error. If the request was successful, it returns
/// `(url, Some(response_content))`.
fn fetch<'a, C: Connect>(
    url: String,
    client: &'a Client<C>,
    encoding: Option<SupportedEncoding>,
) -> Box<Future<Item = (String, Option<Vec<u8>>), Error = Error> + 'a> {
    let uri = Uri::from_str(&url).map_err(|e| Error::from(hyper::Error::from(e)));
    let process_response = move |res: Response| {
        let code = res.status();

        if code == StatusCode::NotFound {
            return Either::A(future::ok((url, None)));
        }

        if !code.is_success() {
            return Either::A(future::err(Error::from(ErrorKind::Http(url, code))));
        }


        // Determine the encoding. Uses the provided encoding or an encoding computed
        // from the response headers.
        let encoding = match encoding.or_else(|| compute_encoding(res.headers())) {
            Some(e) => e,
            None => return Either::A(future::err (
                ErrorKind::UnsupportedEncoding(url, res.headers().get::<ContentEncoding>().cloned()).into()
            )),
        };

        use self::SupportedEncoding::*;

        let decoded = match encoding {
            Xz => {
                let result = res.body()
                    .map_err(Error::from)
                    .fold((url, XzDecoder::new(Vec::new())),
                          move |(url, mut decoder), chunk| {
                        decoder
                            .write_all(&chunk)
                            .chain_err(|| ErrorKind::Decode(url.clone()))
                            .map(move |_| (url, decoder))
                    })
                    .and_then(|(url, mut d)| {
                        d.finish()
                            .chain_err(|| ErrorKind::Decode(url.clone()))
                            .map(move |v| (url, v))
                    });

                Either::A(result)
            }

            Brotli => {
                let result = res.body()
                    .map_err(Error::from)
                    .fold((url, BrotliDecoder::new(Vec::new())),
                          move |(url, mut decoder), chunk| {
                        decoder
                            .write_all(&chunk)
                            .chain_err(|| ErrorKind::Decode(url.clone()))
                            .map(move |_| (url, decoder))
                    })
                    .and_then(|(url, mut d)| {
                        d.finish()
                            .chain_err(|| ErrorKind::Decode(url.clone()))
                            .map(move |v| (url, v))
                    });

                Either::B(Either::A(result))
            }

            Identity => {
                let result = res.body()
                    .map_err(Error::from)
                    .fold(Vec::new(), |mut v, chunk| {
                        v.extend_from_slice(&chunk);
                        Ok(v) as Result<_>
                    })
                    .map(move |r| (url, r));
                Either::B(Either::B(result))
            }
        };


        Either::B(decoded.map(|(url, v)| (url, Some(v))))
    };

    Box::new(future::result(uri)
                 .and_then(move |u| client.get(u).from_err())
                 .and_then(process_response))
}

/// Fetches the references of a given store path.
///
/// Returns the references of the store path and the store path itself. Note that this
/// function only requires the hash part of the store path that is passed as argument,
/// but it will return a full store path as a result. So you can use this function to
/// resolve hashes to full store paths as well.
pub fn fetch_references<'a, C: Connect>
    (
    cache_url: &str,
    client: &'a Client<C>,
    mut path: StorePath,
) -> Box<Future<Item = (StorePath, Vec<StorePath>), Error = Error> + 'a> {
    let url = format!("{}/{}.narinfo", cache_url, path.hash());

    let parse_response = move |(url, data)| {
        let url: String = url;
        let data: Vec<u8> = match data {
            Some(v) => v,
            None => return Ok((path, vec![])),
        };
        let references = b"References:";
        let store_path = b"StorePath:";
        let mut result = Vec::new();
        for line in data.split(|x| x == &b'\n') {
            if line.starts_with(references) {
                let line = &line[references.len()..];
                let line = str::from_utf8(line).map_err(|e| ErrorKind::Unicode(url.clone(), line.to_vec(), e))?;
                result = line.trim()
                    .split_whitespace()
                    .map(|new_path| {
                        let new_origin = PathOrigin {
                            toplevel: false,
                            ..path.origin().into_owned()
                        };
                        StorePath::parse(new_origin, new_path).ok_or_else(|| {
                            ErrorKind::ParseStorePath(url.clone(), new_path.to_string()).into()
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
            }

            if line.starts_with(store_path) {
                let line = &line[references.len()..];
                let line = str::from_utf8(line).map_err(|e| ErrorKind::Unicode(url.clone(), line.to_vec(), e))?;
                let line = line.trim();

                path = StorePath::parse(path.origin().into_owned(), line).ok_or_else(|| ErrorKind::ParseStorePath(url.clone(), line.to_string()))?;
            }
        }

        Ok((path, result))
    };

    Box::new(fetch(url, client, None).and_then(move |x| parse_response(x)))
}

/// Fetches the file listing for the given store path.
///
/// A file listing is a tree of the files that the given store path contains.
pub fn fetch_files<'a, C: Connect>(
    cache_url: &str,
    client: &'a Client<C>,
    path: &StorePath,
) -> Box<Future<Item = Option<FileTree>, Error = Error> + 'a> {
    let url_xz = format!("{}/{}.ls.xz", cache_url, path.hash());
    let url_generic = format!("{}/{}.ls", cache_url, path.hash());
    let name = format!("{}.json", path.hash());

    let fetched = fetch(url_xz, client, Some(SupportedEncoding::Xz)).and_then(move |(url, r)| {
        match r {
            Some(v) => Either::A(future::ok((url, Some(v)))),
            None => Either::B(fetch(url_generic, client, None)),
        }
    });

    let parse_response = move |(url, res)| {
        let url: String = url;
        let res: Option<Vec<u8>> = res;
        let contents = match res {
            None => return Ok(None),
            Some(v) => v,
        };

        let now = Instant::now();
        let response: FileListingResponse = serde_json::from_slice(&contents).chain_err(|| {
                ErrorKind::ParseResponse(url, util::write_temp_file("file_listing.json", &contents))
            })?;
        let duration = now.elapsed();

        if duration > Duration::from_millis(2000) {
            let secs = duration.as_secs();
            let millis = duration.subsec_nanos() / 1000000;

            writeln!(&mut io::stderr(),
                     "warning: took a long time to parse: {}s:{:03}ms",
                     secs,
                     millis)
                    .unwrap_or(());
            if let Some(p) = util::write_temp_file(&name, &contents) {
                writeln!(&mut io::stderr(),
                         "saved response to file: {}",
                         p.to_string_lossy())
                        .unwrap_or(());
            }
        }

        Ok(Some(response.root.0))
    };

    Box::new(fetched.and_then(parse_response))

}

/// This data type represents the format of the `.ls` files fetched from the binary cache.
///
/// The `.ls` file contains a JSON object. The structure of that object is mirrored by this
/// struct for parsing the file.
#[derive(Deserialize, Debug, PartialEq)]
struct FileListingResponse {
    /// Each `.ls` file has a "root" key that contains the file listing.
    root: HydraFileListing,
}

/// A wrapper for `FileTree` so that we can add trait implementations for it.
///
/// (`FileTree` is defined in another module, so we cannot directly implement `Deserialize` for
/// `FileTree` since that would be an orphan impl).
#[derive(Debug, PartialEq)]
struct HydraFileListing(FileTree);

/// We need a manual implementation for Deserialize here because file lisitings can contain non-unicode
/// bytes so we need to explicitly request that keys be deserialized as ByteBuf and not String.
///
/// We cannot use the serde-derive machinery because the `tagged` enum variant does not support map keys
/// that aren't valid unicode (since it relies on the Deserializer to tell it the type, and the JSON Deserializer
/// will default to String for map keys).
impl Deserialize for HydraFileListing {
    fn deserialize<D: Deserializer>(d: D) -> result::Result<HydraFileListing, D::Error> {
        struct Root;

        // The visitor that implements derialization for a file tree
        impl Visitor for Root {
            type Value = FileTree;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a file listing (map)")
            }

            fn visit_map<V: MapVisitor>(
                self,
                mut visitor: V,
            ) -> result::Result<FileTree, V::Error> {
                const VARIANTS: &'static [&'static str] = &["regular", "directory", "symlink"];

                // These will get filled in as we visit the map.
                // Note that not all of them will be available, depending on the `type` of the file listing
                // (`directory`, `symlink` or `regular`)
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
                        }
                        b"size" => {
                            if size.is_some() {
                                return Err(serde::de::Error::duplicate_field("size"));
                            }
                            size = Some(try!(visitor.visit_value()))
                        }
                        b"executable" => {
                            if executable.is_some() {
                                return Err(serde::de::Error::duplicate_field("executable"));
                            }
                            executable = Some(try!(visitor.visit_value()))
                        }
                        b"entries" => {
                            if entries.is_some() {
                                return Err(serde::de::Error::duplicate_field("entries"));
                            }
                            entries = Some(try!(visitor.visit_value()))
                        }
                        b"target" => {
                            if target.is_some() {
                                return Err(serde::de::Error::duplicate_field("target"));
                            }
                            target = Some(try!(visitor.visit_value()))
                        }
                        _ => {
                            // We ignore all other fields to be more robust against changes in
                            // the format
                            try!(visitor.visit_value::<serde::de::impls::IgnoredAny>());
                        }
                    }
                }

                // the type field must always be present so we know which type to expect
                let typ = &try!(typ.ok_or_else(|| serde::de::Error::missing_field("type"))) as
                          &[u8];

                match typ {
                    b"regular" => {
                        let size = size.ok_or_else(|| serde::de::Error::missing_field("size"))?;
                        let executable = executable.unwrap_or(false);
                        Ok(FileTree::regular(size, executable))
                    }
                    b"directory" => {
                        let entries =
                            entries.ok_or_else(|| serde::de::Error::missing_field("entries"))?;
                        let entries = entries.into_iter().map(|(k, v)| (k, v.0)).collect();
                        Ok(FileTree::directory(entries))
                    }
                    b"symlink" => {
                        let target =
                            target.ok_or_else(|| serde::de::Error::missing_field("target"))?;
                        Ok(FileTree::symlink(target))
                    }
                    _ => {
                        Err(serde::de::Error::unknown_variant(&String::from_utf8_lossy(typ),
                                                              VARIANTS))
                    }
                }
            }
        }
        d.deserialize_map(Root).map(HydraFileListing)
    }
}
