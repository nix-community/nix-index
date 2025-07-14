//! Interacting with hydra and the binary cache.
//!
//! This module has all functions that deal with accessing hydra or the binary cache.
//! Currently, it only provides two functions: `fetch_files` to get the file listing for
//! a store path and `fetch_references` to retrieve the references from the narinfo.
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::result;
use std::str::{self, Utf8Error};
use std::time::{Duration, Instant};

use futures::future;
use futures::{Future, TryFutureExt};
use reqwest::header::{HeaderValue, ACCEPT_ENCODING};
use reqwest::Url;
use reqwest::{Client, ClientBuilder, StatusCode};
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::{self, Deserialize};
use serde_bytes::ByteBuf;
use serde_json;
use thiserror::Error;
use tokio::time::error::Elapsed;
use tokio_retry::strategy::ExponentialBackoff;
use tokio_retry::{self, Retry};
use xz2::read::XzDecoder;

use crate::files::FileTree;
use crate::package::{PathOrigin, StorePath};
use crate::util;

#[derive(Error, Debug)]
pub enum Error {
    #[error("request GET '{url}' failed with HTTP error {code}")]
    Http { url: String, code: StatusCode },
    #[error(
        "response to GET '{url}' failed to parse{}",
        tmp_file.as_ref().map_or("".into(), |f| format!(" (response saved to {})", f.to_string_lossy()))
    )]
    ParseResponse {
        url: String,
        tmp_file: Option<PathBuf>,
    },
    #[error("response to GET '{url}' contained invalid store path '{path}', expected string matching format $(NIX_STORE_DIR)$(HASH)-$(NAME)")]
    ParseStorePath { url: String, path: String },
    #[error("response to GET '{url}' contained invalid unicode byte {}: {err}", bytes[err.valid_up_to()])]
    Unicode {
        url: String,
        bytes: Vec<u8>,
        #[source]
        err: Utf8Error,
    },
    #[error("response to GET '{url}' could not be decoded")]
    Decode { url: String },
    #[error(
        "response to GET '{url}' had unsupported content-encoding ({})",
        encoding.as_ref().map_or("not present".to_string(), |v| format!("'{}'", v))
    )]
    UnsupportedEncoding {
        url: String,
        encoding: Option<String>,
    },
    #[error("timeout exceeded")]
    Timeout,
    #[error("timer failure")]
    TimerError,
    #[error("Can not parse proxy url ({url})")]
    ParseProxy { url: String },
    #[error("HTTP client error: {0}")]
    Reqwest(#[from] reqwest::Error),
}

impl From<Elapsed> for Error {
    fn from(_err: Elapsed) -> Self {
        Error::Timeout
    }
}

type Result<T> = std::result::Result<T, Error>;

/// A Fetcher allows you to make requests to Hydra/the binary cache.
///
/// It holds all the relevant state for performing requests, such as for example
/// the HTTP client instance and a timer for timeouts.
///
/// You should use a single instance of this struct to make all your hydra/binary cache
/// requests.
pub struct Fetcher {
    client: Client,
    cache_url: String,
}

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A boxed future using this module's error type.
type BoxFuture<'a, I> = Pin<Box<dyn Future<Output = Result<I>> + 'a>>;

pub struct ParsedNAR {
    pub store_path: StorePath,
    pub nar_path: String,
    pub references: Vec<StorePath>,
}

impl Fetcher {
    /// Initializes a new instance of the `Fetcher` struct.
    ///
    /// The `handle` argument is a Handle to the tokio event loop.
    ///
    /// `cache_url` specifies the URL of the binary cache (example: `https://cache.nixos.org`).
    pub fn new(cache_url: String) -> Result<Fetcher> {
        let client = ClientBuilder::new()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(RESPONSE_TIMEOUT)
            .build()?;
        Ok(Fetcher { client, cache_url })
    }

    /// Sends a GET request to the given URL and decodes the response with the given encoding.
    ///
    /// If `encoding` is `None`, then the encoding will be detected automatically by reading
    /// the `Content-Encoding` header.
    ///
    /// The returned future resolves to `(url, None)` if the server returned a 404 error. On any
    /// other error, the future resolves to an error. If the request was successful, it returns
    /// `(url, Some(response_content))`.
    ///
    /// This function will automatically retry the request a few times to mitigate intermittent network
    /// failures.
    fn fetch(&self, url: String) -> BoxFuture<(String, Option<Vec<u8>>)> {
        let strategy = ExponentialBackoff::from_millis(50)
            .max_delay(Duration::from_millis(5000))
            .take(20)
            // add some jitter
            .map(tokio_retry::strategy::jitter)
            // wait at least 5 seconds, as that is the time that cache.nixos.org caches 500 internal server errors
            .map(|x| x + Duration::from_secs(5));
        Box::pin(Retry::spawn(strategy, move || {
            Box::pin(self.fetch_noretry(url.clone()))
        }))
    }

    /// The implementation of `fetch`, without the retry logic.
    async fn fetch_noretry(&self, url: String) -> Result<(String, Option<Vec<u8>>)> {
        let uri = Url::parse(&url).expect("url passed to fetch must be valid");
        let request = self
            .client
            .get(uri)
            .header(
                ACCEPT_ENCODING,
                HeaderValue::from_static("br, gzip, deflate"),
            )
            .build()
            .expect("HTTP request is valid");

        let res = self.client.execute(request).await?;

        let code = res.status();

        if code == StatusCode::NOT_FOUND {
            return Ok((url, None));
        }

        if !code.is_success() {
            return Err(Error::Http { url, code });
        }

        let decoded = res.bytes().await?.into();

        Ok((url, Some(decoded)))
    }

    /// Fetches the references of a given store path.
    ///
    /// Returns the references of the store path and the store path itself. Note that this
    /// function only requires the hash part of the store path that is passed as argument,
    /// but it will return a full store path as a result. So you can use this function to
    /// resolve hashes to full store paths as well.
    ///
    /// The references will be `None` if no information about the store path could be found
    /// (happens if the narinfo wasn't found which means that hydra didn't build this path).
    pub fn fetch_references(&self, mut path: StorePath) -> BoxFuture<Option<ParsedNAR>> {
        let url = format!("{}/{}.narinfo", self.cache_url, path.hash());

        let parse_response = move |(url, data)| {
            let url: String = url;
            let data: Vec<u8> = match data {
                Some(v) => v,
                None => return Ok(None),
            };

            let mut nar_path = None;
            let mut result = Vec::new();
            for line in data.split(|x| x == &b'\n') {
                if let Some(line) = line.strip_prefix(b"References: ") {
                    let line = str::from_utf8(line).map_err(|e| Error::Unicode {
                        url: url.clone(),
                        bytes: line.to_vec(),
                        err: e,
                    })?;
                    result = line
                        .split_whitespace()
                        .map(|new_path| {
                            let new_origin = PathOrigin {
                                toplevel: false,
                                ..path.origin().into_owned()
                            };
                            StorePath::parse(new_origin, new_path).ok_or_else(|| {
                                Error::ParseStorePath {
                                    url: url.clone(),
                                    path: new_path.to_string(),
                                }
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                }

                if let Some(line) = line.strip_prefix(b"StorePath: ") {
                    let line = str::from_utf8(line).map_err(|e| Error::Unicode {
                        url: url.clone(),
                        bytes: line.to_vec(),
                        err: e,
                    })?;
                    let line = line.trim();

                    path = StorePath::parse(path.origin().into_owned(), line).ok_or_else(|| {
                        Error::ParseStorePath {
                            url: url.clone(),
                            path: line.to_string(),
                        }
                    })?;
                }

                if let Some(line) = line.strip_prefix(b"URL: ") {
                    let line = str::from_utf8(line).map_err(|e| Error::Unicode {
                        url: url.clone(),
                        bytes: line.to_vec(),
                        err: e,
                    })?;
                    let line = line.trim();

                    nar_path = Some(line.to_owned());
                }
            }

            Ok(Some(ParsedNAR {
                store_path: path,
                nar_path: nar_path.ok_or(Error::ParseStorePath {
                    url,
                    path: "no URL line found".into(),
                })?,
                references: result,
            }))
        };

        Box::pin(
            self.fetch(url)
                .and_then(|r| future::ready(parse_response(r))),
        )
    }

    /// Fetches the file listing for the given store path.
    ///
    /// A file listing is a tree of the files that the given store path contains.
    pub async fn fetch_files(&self, path: &StorePath) -> Result<Option<FileTree>> {
        let url_xz = format!("{}/{}.ls.xz", self.cache_url, path.hash());
        let url_generic = format!("{}/{}.ls", self.cache_url, path.hash());
        let name = format!("{}.json", path.hash());

        let (url, body) = self.fetch(url_generic).await?;
        let contents = match body {
            Some(v) => v,
            None => {
                let (_, Some(body)) = self.fetch(url_xz.clone()).await? else {
                    return Ok(None);
                };

                let mut unpacked = vec![];
                XzDecoder::new(&body[..])
                    .read_to_end(&mut unpacked)
                    .map_err(|e| Error::Decode { url: e.to_string() })?;

                unpacked
            }
        };

        let now = Instant::now();
        let response: FileListingResponse =
            serde_json::from_slice(&contents[..]).map_err(|_| Error::ParseResponse {
                url,
                tmp_file: util::write_temp_file("file_listing.json", &contents),
            })?;
        let duration = now.elapsed();

        if duration > Duration::from_millis(2000) {
            let secs = duration.as_secs();
            let millis = duration.subsec_millis();

            writeln!(
                &mut io::stderr(),
                "warning: took a long time to parse: {}s:{:03}ms",
                secs,
                millis
            )
            .unwrap_or(());
            if let Some(p) = util::write_temp_file(&name, &contents) {
                writeln!(
                    &mut io::stderr(),
                    "saved response to file: {}",
                    p.to_string_lossy()
                )
                .unwrap_or(());
            }
        }

        Ok(Some(response.root.0))
    }
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
/// bytes so we need to explicitly request that keys be deserialized as `ByteBuf` and not String.
///
/// We cannot use the serde-derive machinery because the `tagged` enum variant does not support map keys
/// that aren't valid unicode (since it relies on the Deserializer to tell it the type, and the JSON Deserializer
/// will default to String for map keys).
impl<'de> Deserialize<'de> for HydraFileListing {
    fn deserialize<D: Deserializer<'de>>(d: D) -> result::Result<HydraFileListing, D::Error> {
        struct Root;

        // The access that implements derialization for a file tree
        impl<'de> Visitor<'de> for Root {
            type Value = FileTree;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a file listing (map)")
            }

            fn visit_map<V: MapAccess<'de>>(
                self,
                mut access: V,
            ) -> result::Result<FileTree, V::Error> {
                const VARIANTS: &[&str] = &["regular", "directory", "symlink"];

                // These will get filled in as we visit the map.
                // Note that not all of them will be available, depending on the `type` of the file listing
                // (`directory`, `symlink` or `regular`)
                let mut typ: Option<ByteBuf> = None;
                let mut size: Option<u64> = None;
                let mut executable: Option<bool> = None;
                let mut entries: Option<HashMap<ByteBuf, HydraFileListing>> = None;
                let mut target: Option<ByteBuf> = None;

                while let Some(key) = access.next_key::<ByteBuf>()? {
                    match &key as &[u8] {
                        b"type" => {
                            if typ.is_some() {
                                return Err(serde::de::Error::duplicate_field("type"));
                            }
                            typ = Some(access.next_value()?)
                        }
                        b"size" => {
                            if size.is_some() {
                                return Err(serde::de::Error::duplicate_field("size"));
                            }
                            size = Some(access.next_value()?)
                        }
                        b"executable" => {
                            if executable.is_some() {
                                return Err(serde::de::Error::duplicate_field("executable"));
                            }
                            executable = Some(access.next_value()?)
                        }
                        b"entries" => {
                            if entries.is_some() {
                                return Err(serde::de::Error::duplicate_field("entries"));
                            }
                            entries = Some(access.next_value()?)
                        }
                        b"target" => {
                            if target.is_some() {
                                return Err(serde::de::Error::duplicate_field("target"));
                            }
                            target = Some(access.next_value()?)
                        }
                        _ => {
                            // We ignore all other fields to be more robust against changes in
                            // the format
                            access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                // the type field must always be present so we know which type to expect
                let typ: &[u8] = &typ.ok_or_else(|| serde::de::Error::missing_field("type"))?;

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
                    _ => Err(serde::de::Error::unknown_variant(
                        &String::from_utf8_lossy(typ),
                        VARIANTS,
                    )),
                }
            }
        }
        d.deserialize_map(Root).map(HydraFileListing)
    }
}
