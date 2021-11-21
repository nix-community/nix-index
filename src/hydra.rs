//! Interacting with hydra and the binary cache.
//!
//! This module has all functions that deal with accessing hydra or the binary cache.
//! Currently, it only provides two functions: `fetch_files` to get the file listing for
//! a store path and `fetch_references` to retrieve the references from the narinfo.
use serde;
use serde_json;

use brotli2::write::BrotliDecoder;
use futures::future::{self, Either};
use futures::{Future, TryFutureExt};
use headers::{Authorization, HeaderValue};
use hyper::client::{Client as HyperClient, HttpConnector};
use hyper::{self, Body, Request, StatusCode, Uri};
use hyper_proxy::{Custom, Intercept, Proxy, ProxyConnector};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_bytes::ByteBuf;
use std::collections::HashMap;
use std::env::var;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::result;
use std::str::{self, FromStr, Utf8Error};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::time::error::Elapsed;
use tokio::time::timeout;
use tokio_retry::strategy::ExponentialBackoff;
use tokio_retry::{self, Retry};
use tokio_stream::StreamExt;
use url::Url;
use xz2::write::XzDecoder;

use crate::files::FileTree;
use crate::package::{PathOrigin, StorePath};
use crate::util;

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
        UnsupportedEncoding(url: String, encoding: Option<String>) {
            description("unsupported content-encoding")
            display(
                "response to GET '{}' had unsupported content-encoding ({})",
                url,
                encoding.as_ref().map_or("not present".to_string(), |v| format!("'{}'", v)),
            )
        }
        Timeout {
            description("timeout exceeded")
        }
        TimerError {
            description("timer failure")
        }
        ParseProxy(url: String) {
            description("proxy config error")
            display("Can not parse proxy url ({})", url)
        }
    }
    foreign_links {
        Hyper(hyper::Error);
    }
}

impl From<Elapsed> for Error {
    fn from(_err: Elapsed) -> Self {
        return Error::from(ErrorKind::Timeout);
    }
}

enum Client {
    Proxy(
        HyperClient<ProxyConnector<HttpConnector>>,
        ProxyConnector<HttpConnector>,
    ),
    NoProxy(HyperClient<HttpConnector>),
}

impl Client {
    pub fn new(_handle: &Handle) -> Result<Client> {
        let connector = HttpConnector::new();
        let http_proxy = var("HTTP_PROXY");

        match http_proxy {
            Ok(proxy_url) => {
                let mut url =
                    Url::parse(&proxy_url).map_err(|_| ErrorKind::ParseProxy(proxy_url.clone()))?;
                let username = String::from(url.username()).clone();
                let password = url.password().map(|pw| String::from(pw));

                url.set_username("")
                    .map_err(|_| ErrorKind::ParseProxy(proxy_url.clone()))?;
                url.set_password(None)
                    .map_err(|_| ErrorKind::ParseProxy(proxy_url.clone()))?;

                // No need to check for the error. Because Url::parse()? already checked it.
                let uri = url.to_string().parse().unwrap();

                let intercept = match var("NO_PROXY") {
                    Ok(urls) => Intercept::Custom(Custom::from(
                        move |_scheme: Option<&str>, host: Option<&str>, _port: Option<u16>| {
                            let url_list = urls.split(",");
                            !url_list.into_iter().any(|pat_str| {
                                let pat_str = pat_str.trim();

                                if pat_str == "*" {
                                    true
                                } else {
                                    let pat_uri = hyper::Uri::from_str(&pat_str);

                                    match host {
                                        Some(host) => {
                                            if let Ok(pat_uri) = pat_uri {
                                                let pat_host = pat_uri.host();

                                                if let Some(pat_host) = pat_host {
                                                    host.ends_with(&format!(".{}", pat_host))
                                                        || host == pat_host
                                                } else {
                                                    false
                                                }
                                            } else {
                                                false
                                            }
                                        }
                                        None => true,
                                    }
                                }
                            })
                        },
                    )),
                    Err(_) => Intercept::All,
                };

                let mut proxy = Proxy::new(intercept, uri);

                if username != "" {
                    proxy.set_authorization(Authorization::basic(
                        &username,
                        &password.unwrap_or_default(),
                    ));
                }

                let proxy_connector = hyper_proxy::ProxyConnector::from_proxy(connector, proxy)
                    .map_err(|_| ErrorKind::ParseProxy(proxy_url.clone()))?;

                Ok(Client::Proxy(
                    hyper::Client::builder().build(proxy_connector.clone()),
                    proxy_connector,
                ))
            }
            Err(_) => Ok(Client::NoProxy(HyperClient::builder().build(connector))),
        }
    }

    pub fn request(
        &self,
        req: hyper::Request<hyper::Body>,
    ) -> hyper::client::ResponseFuture {
        let mut req = req;
        match self {
            Client::Proxy(client, connector) => {
                if let Some(headers) = connector.http_headers(&req.uri()) {
                    req.headers_mut().extend(headers.clone().into_iter());
                }
                client.request(req)
            }
            Client::NoProxy(client) => client.request(req),
        }
    }
}

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
    handle: Handle,
}

const RESPONSE_TIMEOUT_MS: u64 = 1000;
const CONNECT_TIMEOUT_MS: u64 = 10000;

/// A boxed future using this module's error type.
type BoxFuture<'a, I> = Pin<Box<dyn Future<Output = Result<I>> + 'a>>;

impl Fetcher {
    /// Initializes a new instance of the `Fetcher` struct.
    ///
    /// The `handle` argument is a Handle to the tokio event loop.
    ///
    /// `cache_url` specifies the URL of the binary cache (example: `https://cache.nixos.org`).
    pub fn new(
        cache_url: String,
        handle: Handle,
    ) -> Result<Fetcher> {
        let client = Client::new(&handle)?;
        Ok(Fetcher {
            client: client,
            cache_url: cache_url,
            handle: handle,
        })
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
    fn fetch(
        &self,
        url: String,
        encoding: Option<SupportedEncoding>,
    ) -> BoxFuture<(String, Option<Vec<u8>>)> {
        let strategy = ExponentialBackoff::from_millis(50)
            .max_delay(Duration::from_millis(5000))
            .take(20)
            // add some jitter
            .map(|x| tokio_retry::strategy::jitter(x))
            // wait at least 5 seconds, as that is the time that cache.nixos.org caches 500 internal server errors
            .map(|x| x + Duration::from_secs(5));
        Box::pin(Retry::spawn(strategy, move || {
            Box::pin(self.fetch_noretry(url.clone(), encoding))
        }))
    }

    /// The implementation of `fetch`, without the retry logic.
    async fn fetch_noretry(
        &self,
        url: String,
        encoding: Option<SupportedEncoding>,
    ) -> Result<(String, Option<Vec<u8>>)> {
        let uri = Uri::from_str(&url).expect("url passed to fetch must be valid");
        let request = Request::get(uri)
            .header(
                hyper::header::ACCEPT_ENCODING,
                HeaderValue::from_static("br, gzip, deflate"),
            )
            .body(Body::empty())
            .expect("hyper HTTP request is valid");

        let res = timeout(
            Duration::from_millis(CONNECT_TIMEOUT_MS),
            self.client.request(request),
        )
        .await??;

        let code = res.status();

        if code == StatusCode::NOT_FOUND {
            return Ok((url, None));
        }

        if !code.is_success() {
            return Err(Error::from(ErrorKind::Http(url, code)));
        }

        // Determine the encoding. Uses the provided encoding or an encoding computed
        // from the response headers.
        let encoding = encoding.map(Ok).unwrap_or_else(|| {
            compute_encoding(res.headers())
                .map_err(|e| ErrorKind::UnsupportedEncoding(url.clone(), Some(e)))
        })?;

        let mut content = Box::pin(
            res.into_body()
                .timeout(Duration::from_millis(RESPONSE_TIMEOUT_MS)),
        );

        use self::SupportedEncoding::*;
        let decoded = match encoding {
            Xz => {
                let mut decoder = XzDecoder::new(Vec::new());
                while let Some(v) = content.next().await {
                    let v = v.map_err(|_e| ErrorKind::Timeout)??;
                    decoder
                        .write_all(&v)
                        .chain_err(|| ErrorKind::Decode(url.clone()))?;
                }

                decoder
                    .finish()
                    .chain_err(|| ErrorKind::Decode(url.clone()))?
            }

            Brotli => {
                let mut decoder = BrotliDecoder::new(Vec::new());
                while let Some(v) = content.next().await {
                    let v = v.map_err(|_e| ErrorKind::Timeout)??;
                    decoder
                        .write_all(&v)
                        .chain_err(|| ErrorKind::Decode(url.clone()))?;
                }

                decoder
                    .finish()
                    .chain_err(|| ErrorKind::Decode(url.clone()))?
            }

            Identity => {
                let mut out = Vec::new();
                while let Some(v) = content.next().await {
                    let v = v.map_err(|_e| ErrorKind::Timeout)??;
                    out.extend_from_slice(&v);
                }
                out
            }
        };

        return Ok((url, Some(decoded)));
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
    pub fn fetch_references(
        &self,
        mut path: StorePath,
    ) -> BoxFuture<(StorePath, Option<Vec<StorePath>>)> {
        let url = format!("{}/{}.narinfo", self.cache_url, path.hash());

        let parse_response = move |(url, data)| {
            let url: String = url;
            let data: Vec<u8> = match data {
                Some(v) => v,
                None => return Ok((path, None)),
            };
            let references = b"References:";
            let store_path = b"StorePath:";
            let mut result = Vec::new();
            for line in data.split(|x| x == &b'\n') {
                if line.starts_with(references) {
                    let line = &line[references.len()..];
                    let line = str::from_utf8(line)
                        .map_err(|e| ErrorKind::Unicode(url.clone(), line.to_vec(), e))?;
                    result = line
                        .trim()
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
                    let line = str::from_utf8(line)
                        .map_err(|e| ErrorKind::Unicode(url.clone(), line.to_vec(), e))?;
                    let line = line.trim();

                    path = StorePath::parse(path.origin().into_owned(), line)
                        .ok_or_else(|| ErrorKind::ParseStorePath(url.clone(), line.to_string()))?;
                }
            }

            Ok((path, Some(result)))
        };

        Box::pin(
            self.fetch(url, None)
                .and_then(|r| future::ready(parse_response(r))),
        )
    }

    /// Fetches the file listing for the given store path.
    ///
    /// A file listing is a tree of the files that the given store path contains.
    pub fn fetch_files<'a>(
        &'a self,
        path: &StorePath,
    ) -> Pin<Box<dyn Future<Output = Result<Option<FileTree>>> + 'a>> {
        let url_xz = format!("{}/{}.ls.xz", self.cache_url, path.hash());
        let url_generic = format!("{}/{}.ls", self.cache_url, path.hash());
        let name = format!("{}.json", path.hash());

        let fetched = self
            .fetch(url_generic, None)
            .and_then(move |(url, r)| match r {
                Some(v) => Either::Left(future::ok((url, Some(v)))),
                None => Either::Right(self.fetch(url_xz, Some(SupportedEncoding::Xz))),
            });

        let parse_response = move |(url, res)| {
            let url: String = url;
            let res: Option<Vec<u8>> = res;
            let contents = match res {
                None => return Ok(None),
                Some(v) => v,
            };

            let now = Instant::now();
            let response: FileListingResponse =
                serde_json::from_slice(&contents).chain_err(|| {
                    ErrorKind::ParseResponse(
                        url,
                        util::write_temp_file("file_listing.json", &contents),
                    )
                })?;
            let duration = now.elapsed();

            if duration > Duration::from_millis(2000) {
                let secs = duration.as_secs();
                let millis = duration.subsec_nanos() / 1000000;

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
        };

        Box::pin(fetched.and_then(move |v| {
            let parse_result = parse_response(v);
            future::ready(parse_result)
        }))
    }
}

/// This enum lists the compression algorithms that we support for responses from hydra.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
fn compute_encoding(
    headers: &hyper::HeaderMap
) -> ::std::result::Result<SupportedEncoding, String> {
    let encodings: Vec<_> = headers
        .get_all(hyper::header::CONTENT_ENCODING)
        .into_iter()
        .flat_map(|v| v.as_bytes().split(|c| *c == b','))
        .collect();

    if encodings.len() > 1 {
        return Err(String::from_utf8_lossy(&encodings.join(&b", "[..])).into_owned());
    }

    let encoding = match encodings.get(0) {
        None => return Ok(SupportedEncoding::Identity),
        Some(v) => *v,
    };

    match encoding {
        b"br" => Ok(SupportedEncoding::Brotli),
        b"identity" | b"" => Ok(SupportedEncoding::Identity),
        b"xz" => Ok(SupportedEncoding::Xz),
        _ => Err(String::from_utf8_lossy(&encodings.join(&b", "[..])).into_owned()),
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

            fn expecting(
                &self,
                f: &mut fmt::Formatter,
            ) -> fmt::Result {
                write!(f, "a file listing (map)")
            }

            fn visit_map<V: MapAccess<'de>>(
                self,
                mut access: V,
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
                let typ: &[u8] = &*typ.ok_or_else(|| serde::de::Error::missing_field("type"))?;

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
