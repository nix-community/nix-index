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

enum SupportedEncoding {
    Xz,
    Brotli,
    Identity,
}

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

        let encoding = match encoding.or_else(|| compute_encoding(res.headers())) {
            Some(e) => e,
            None => return Either::A(future::err (
                ErrorKind::UnsupportedEncoding(url, res.headers().get::<ContentEncoding>().cloned()).into()
            )),
        };

        use self::SupportedEncoding::*;
        let decoded = match encoding {
            Xz => {
                let body = res.body().map_err(Error::from);
                Either::A(body.fold((url, XzDecoder::new(Vec::new())),
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
                                        }))
            }

            Brotli => {
                let body = res.body().map_err(Error::from);

                Either::B(Either::A(body.fold((url, BrotliDecoder::new(Vec::new())),
                                              move |(url, mut decoder), chunk| {
                                                  decoder
                                                      .write_all(&chunk)
                                                      .chain_err(|| {
                                                                     ErrorKind::Decode(url.clone())
                                                                 })
                                                      .map(move |_| (url, decoder))
                                              })
                                        .and_then(|(url, mut d)| {
                                                      d.finish()
                            .chain_err(|| ErrorKind::Decode(url.clone()))
                            .map(move |v| (url, v))
                                                  })))
            }

            Identity => {
                let body = res.body().map_err(Error::from);
                let decoded = body.fold(Vec::new(), |mut v, chunk| {
                    v.extend_from_slice(&chunk);
                    Ok(v) as Result<_>
                });
                Either::B(Either::B(decoded.map(move |r| (url, r))))
            }
        };


        Either::B(decoded.map(|(url, v)| (url, Some(v))))
    };

    Box::new(future::result(uri)
                 .and_then(move |u| client.get(u).from_err())
                 .and_then(process_response))
}

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
                result = line.trim().split_whitespace().map(|new_path| {
                    let new_origin = PathOrigin { toplevel: false, ..path.origin().into_owned() };
                    StorePath::parse(new_origin, new_path).ok_or_else(|| ErrorKind::ParseStorePath(url.clone(), new_path.to_string()).into())
                }).collect::<Result<Vec<_>>>()?;
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

pub fn fetch_files<'a, C: Connect>(
    cache_url: &str,
    client: &'a Client<C>,
    path: &StorePath,
) -> Box<Future<Item = Option<FileTree>, Error = Error> + 'a> {
    let url_xz = format!("{}/{}.ls.xz", cache_url, path.hash());
    let url_generic = format!("{}/{}.ls", cache_url, path.hash());
    let name = format!("{}.json", path.hash());

    let fetched = fetch(url_xz, client, Some(SupportedEncoding::Xz)).and_then(move |(url, r)| match r {
        Some(v) => Either::A(future::ok((url, Some(v)))),
        None => Either::B(fetch(url_generic, client, None)),
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
                           ErrorKind::ParseResponse(url,
                                                    util::write_temp_file("file_listing.json",
                                                                          &contents))
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

#[derive(Deserialize, Debug, PartialEq)]
struct FileListingResponse {
    root: HydraFileListing,
}

#[derive(Debug, PartialEq)]
struct HydraFileListing(FileTree);

impl Deserialize for HydraFileListing {
    fn deserialize<D: Deserializer>(d: D) -> result::Result<HydraFileListing, D::Error> {
        struct Root;

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
                            try!(visitor.visit_value::<serde::de::impls::IgnoredAny>());
                        }
                    }
                }

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
