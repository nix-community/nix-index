//! Data types for representing meta information about packages and store paths.
//!
//! The main data type in this `StorePath`, which represents a single output of
//! some nix derivation. We also sometimes call a `StorePath` a package, to avoid
//! confusion with file paths.
use std::borrow::Cow;
use std::io::{self, Write};
use std::str;

use serde::{Deserialize, Serialize};

/// A type for describing how to reach a given store path.
///
/// When building an index, we collect store paths from various sources, such
/// as the output of nix-env -qa and the references of those store paths.
///
/// To show the user how we reached a given store path, each store path tracks
/// its origin. For example, for top-level store paths, we know which attribute
/// of nixpkgs builds this store path.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathOrigin {
    /// The attribute of nixpkgs that lead to this store path being discovered.
    ///
    /// If the store path is a top-level path, then the store path corresponds
    /// to an output of the derivation assigned to this attribute path.
    pub attr: String,

    /// The output of the derivation specified by `attr` that we want to refer to.
    ///
    /// If a derivation does not support multiple outputs, then this should just be "out",
    /// the default output.
    pub output: String,

    /// Indicates that this path is listed in the output of nix-env -qaP --out-name.
    ///
    /// We may index paths for which we do not know the exact attribute path. In this
    /// case, `attr` and `output` will be set to the values for the top-level path that
    /// contains the path in its closure. (This is also how we discovered the path in the
    /// first place: through being referenced by another, top-level path). It is unspecified
    /// which top-level path they will refer to though if there exist multiple ones whose closure
    /// contains this path.
    pub toplevel: bool,

    /// Target system
    pub system: Option<String>,
}

impl PathOrigin {
    /// Encodes a path origin as a sequence of bytes, such that it can be decoed using `decode`.
    ///
    /// The encoding does not use the bytes `0x00` nor `0x01`, as long as neither `attr` nor `output`
    /// contain them. This is important since it allows the result to be encoded with [frcode](mod.frcode.html).
    ///
    /// # Panics
    ///
    /// The `attr` and `output` of the path origin must not contain the byte value `0x02`, otherwise
    /// this function panics.
    ///
    /// # Errors
    ///
    /// Returns any errors that were encountered while writing to the supplied `Writer`.
    pub fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        assert!(
            !self.attr.contains('\x02'),
            "origin attribute path must not contain the byte value 0x02 anywhere"
        );
        assert!(
            !self.output.contains('\x02'),
            "origin output name must not contain the byte value 0x02 aynwhere"
        );
        write!(
            writer,
            "{}\x02{}{}",
            self.attr,
            self.output,
            if self.toplevel { "" } else { "\x02" }
        )?;
        Ok(())
    }

    /// Decodes a path that was encoded by `encode` function of this trait.
    ///
    /// Returns the decoded path origin, or `None` if `buf` could not be decoded as path origin.
    pub fn decode(buf: &[u8]) -> Option<PathOrigin> {
        let mut iter = buf.splitn(2, |c| *c == b'\x02');
        iter.next()
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .and_then(|attr| {
                iter.next()
                    .and_then(|v| String::from_utf8(v.to_vec()).ok())
                    .and_then(|mut output| {
                        let mut toplevel = true;
                        if let Some(l) = output.pop() {
                            if l == '\x02' {
                                toplevel = false
                            } else {
                                output.push(l)
                            }
                        }
                        Some(PathOrigin {
                            attr: attr,
                            output: output,
                            toplevel: toplevel,
                            system: None,
                        })
                    })
            })
    }
}

/// Represents a store path which is something that is produced by `nix-build`.
///
/// A store path represents an output in the nix store, matching the pattern
/// `store_dir/hash-name` (most often, `store_dir` will be `/nix/store`).
///
/// Using nix, a store path can be produced by calling `nix-build`.
///
/// Note that even if a store path is a directory, the files inside that directory
/// themselves are *not* store paths. For example, while the following is a store path:
///
/// ```text
/// /nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5
/// ````
///
/// while this is not:
///
/// ```text
/// /nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5/bin/
/// ```
///
/// To avoid any confusion with file paths, we sometimes also refer to a store path as a *package*.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    store_dir: String,
    hash: String,
    name: String,
    origin: PathOrigin,
}

impl StorePath {
    /// Parse a store path from an absolute file path.
    ///
    /// Since this function does not know where that path comes from, it takes
    /// `origin` as an argument.
    ///
    /// This function returns `None` if the path could not be parsed as a
    /// store path. You should not rely on that to check whether a path is a store
    /// path though, since it only does minimal validation (for one example, it does
    /// not check the length of the hash).
    pub fn parse(origin: PathOrigin, path: &str) -> Option<StorePath> {
        let mut parts = path.splitn(2, '-');
        parts.next().and_then(|prefix| {
            parts.next().and_then(|name| {
                let mut iter = prefix.rsplitn(2, '/');
                iter.next().map(|hash| {
                    let store_dir = iter.next().unwrap_or("");
                    StorePath {
                        store_dir: store_dir.to_string(),
                        hash: hash.to_string(),
                        name: name.to_string(),
                        origin: origin,
                    }
                })
            })
        })
    }

    /// Encodes a store path as a sequence of bytes, so that it can be decoded with `decode`.
    ///
    /// The encoding does not use the bytes `0x00` nor `0x01`, as long as none of the fields of
    /// this path contain those bytes (this includes `store_dir`, `hash`, `name` and `origin`).
    /// This is important since it allows the result to be encoded with [frcode](mod.frcode.html).
    ///
    /// # Panics
    ///
    /// The `attr` and `output` of the path origin must not contain the byte value `0x02`, otherwise
    /// this function panics.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut result = Vec::with_capacity(self.as_str().len());
        result.extend(self.as_str().bytes());
        result.push(b'\n');
        self.origin().encode(&mut result)?;
        Ok(result)
    }

    pub fn decode(buf: &[u8]) -> Option<StorePath> {
        let mut parts = buf.splitn(2, |c| *c == b'\n');
        parts
            .next()
            .and_then(|v| str::from_utf8(v).ok())
            .and_then(|path| {
                parts
                    .next()
                    .and_then(PathOrigin::decode)
                    .and_then(|origin| StorePath::parse(origin, path))
            })
    }

    /// Returns the name of the store path, which is the part of the file name that
    /// is not the hash.  In the above example, it would be `bash-4.4-p5`.
    ///
    /// # Example
    ///
    /// ```
    /// use nix_index::package::{PathOrigin, StorePath};
    ///
    /// let origin = PathOrigin { attr: "dummy".to_string(), output: "out".to_string(), toplevel: true, system: None };
    /// let store_path = StorePath::parse(origin, "/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5").unwrap();
    /// assert_eq!(&store_path.name(), "bash-4.4-p5");
    /// ```
    pub fn name(&self) -> Cow<str> {
        Cow::Borrowed(&self.name)
    }

    /// The hash of the store path. This is the part just before the name of
    /// the path.
    ///
    /// # Example
    ///
    /// ```
    /// use nix_index::package::{PathOrigin, StorePath};
    ///
    /// let origin = PathOrigin { attr: "dummy".to_string(), output: "out".to_string(), toplevel: true, system: None };
    /// let store_path = StorePath::parse(origin, "/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5").unwrap();
    /// assert_eq!(&store_path.name(), "bash-4.4-p5");
    /// ```
    pub fn hash(&self) -> Cow<str> {
        Cow::Borrowed(&self.hash)
    }

    /// The store dir for which this store path was built.
    ///
    /// Currently, this will be `/nix/store` in almost all cases, but
    /// we include it here anyway for completeness.
    ///
    /// # Example
    ///
    /// ```
    /// use nix_index::package::{PathOrigin, StorePath};
    ///
    /// let origin = PathOrigin { attr: "dummy".to_string(), output: "out".to_string(), toplevel: true, system: None };
    /// let store_path = StorePath::parse(origin, "/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5").unwrap();
    /// assert_eq!(&store_path.store_dir(), "/nix/store");
    /// ```
    pub fn store_dir(&self) -> Cow<str> {
        Cow::Borrowed(&self.store_dir)
    }

    /// Converts the store path back into an absolute path.
    ///
    /// # Example
    ///
    /// ```
    /// use nix_index::package::{PathOrigin, StorePath};
    ///
    /// let origin = PathOrigin { attr: "dummy".to_string(), output: "out".to_string(), toplevel: true, system: None };
    /// let store_path = StorePath::parse(origin, "/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5").unwrap();
    /// assert_eq!(&store_path.as_str(), "/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5");
    /// ```
    pub fn as_str(&self) -> Cow<str> {
        Cow::Owned(format!("{}/{}-{}", self.store_dir, self.hash, self.name))
    }

    /// Returns the origin that describes how we discovered this store path.
    ///
    /// See the documentation of `PathOrigin` for more information about this field.
    ///
    /// # Example
    ///
    /// ```
    /// use nix_index::package::{PathOrigin, StorePath};
    ///
    /// let origin = PathOrigin { attr: "dummy".to_string(), output: "out".to_string(), toplevel: true, system: None };
    /// let store_path = StorePath::parse(origin.clone(), "/nix/store/010yd8jls8w4vcnql4zhjbnyp2yay5pl-bash-4.4-p5").unwrap();
    /// assert_eq!(store_path.origin().as_ref(), &origin);
    /// ```
    pub fn origin(&self) -> Cow<PathOrigin> {
        Cow::Borrowed(&self.origin)
    }
}
