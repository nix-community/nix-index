use std::fs::File;
/// Creating and searching file databases.
///
/// This module implements an abstraction for creating an index of files with meta information
/// and searching that index for paths matching a specific pattern.
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use grep;
use grep::matcher::{LineMatchKind, Match, Matcher, NoError};
use memchr::{memchr, memrchr};
use rayon::prelude::*;
use regex::bytes::Regex;
use regex_syntax::ast::{AssertionKind, Ast, Literal};
use serde_json;
use thiserror::Error;
use zstd;

use crate::files::{FileTree, FileTreeEntry};
use crate::frcode;
use crate::package::StorePath;

/// Magic identifying a nix-index database file.
const FILE_MAGIC: &[u8] = b"NIXI";

/// Offset of the first data frame: the magic plus the 8-byte version.
const DATA_START: usize = FILE_MAGIC.len() + 8;

/// zstd skippable-frame magic used to embed the seek table. Skippable frames are ignored by
/// standard zstd decoders, so the database stays readable with plain `zstd -d`.
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;

/// A writer for creating a new file database.
///
/// The file is a sequence of independently-compressed zstd frames followed by a seek table
/// embedded in a skippable frame. Frames are only cut at package boundaries, so each frame is
/// a self-contained frcode stream that can be decoded on its own.
pub struct Writer {
    file: BufWriter<File>,
    level: i32,
    /// Format version to write: 2 (parallel frames) or 1 (legacy single frame).
    version: u64,
    /// All frcode-encoded package data accumulated so far, concatenated.
    data: Vec<u8>,
    /// Offset into `data` of every package boundary; frames may only be cut here.
    boundaries: Vec<usize>,
}

impl Writer {
    /// Creates a new database at the given path with the specified zstd compression level
    /// (currently, supported values range from 0 to 22) and format `version` (1 or 2).
    ///
    /// Version 1 writes the legacy single-frame format for compatibility with older
    /// `nix-locate` binaries; version 2 splits the data into parallel-searchable frames.
    pub fn create<P: AsRef<Path>>(path: P, level: i32, version: u64) -> io::Result<Writer> {
        assert!(version == 1 || version == 2, "unsupported format version");
        let mut file = File::create(path)?;
        file.write_all(FILE_MAGIC)?;
        file.write_u64::<LittleEndian>(version)?;

        Ok(Writer {
            file: BufWriter::new(file),
            level,
            version,
            data: Vec::new(),
            boundaries: Vec::new(),
        })
    }

    /// Add a package's file tree under `path`. Entries are only added if they match
    /// `filter_prefix`.
    pub fn add(
        &mut self,
        path: StorePath,
        files: FileTree,
        filter_prefix: &[u8],
    ) -> io::Result<()> {
        let entries = files.to_list(filter_prefix);

        // Don't add packages with no file entries to the database.
        if entries.is_empty() {
            return Ok(());
        }

        {
            let mut encoder = frcode::Encoder::new(
                &mut self.data,
                b"p".to_vec(),
                serde_json::to_vec(&path).expect("failed to serialize path"),
            );
            for entry in entries {
                entry.encode(&mut encoder)?;
            }
            // Dropping the encoder writes the footer, resetting frcode's shared-prefix state,
            // so this offset is a valid frame boundary.
        }
        self.boundaries.push(self.data.len());
        Ok(())
    }

    /// Splits the accumulated data into one frame per CPU, cutting only at package boundaries
    /// so each frame is a self-contained frcode stream. One frame per core is enough to keep
    /// all cores busy during a query while keeping frames large for a good compression ratio.
    fn frame_ranges(&self) -> Vec<(usize, usize)> {
        if self.boundaries.is_empty() {
            return Vec::new();
        }
        let target_size = (self.data.len() / num_cpus::get().max(1)).max(1);

        let mut ranges = Vec::new();
        let mut start = 0;
        for &boundary in &self.boundaries {
            if boundary - start >= target_size {
                ranges.push((start, boundary));
                start = boundary;
            }
        }
        if start < self.data.len() {
            ranges.push((start, self.data.len()));
        }
        ranges
    }

    /// Writes the seek table as a trailing zstd skippable frame. Payload (little endian):
    /// `frame_count: u32`, `compressed_len: u32` per frame, then `payload_len: u32` again as a
    /// trailer so the reader can locate the frame from the end of the file.
    fn write_seek_table(&mut self, frame_lens: &[u32]) -> io::Result<()> {
        let payload_len = 4 + frame_lens.len() * 4 + 4;
        self.file.write_u32::<LittleEndian>(SKIPPABLE_MAGIC)?;
        self.file.write_u32::<LittleEndian>(payload_len as u32)?;
        self.file
            .write_u32::<LittleEndian>(frame_lens.len() as u32)?;
        for len in frame_lens {
            self.file.write_u32::<LittleEndian>(*len)?;
        }
        self.file.write_u32::<LittleEndian>(payload_len as u32)?;
        Ok(())
    }

    /// Compresses everything into a single zstd frame, the legacy version 1 layout.
    fn finish_v1(&mut self) -> io::Result<()> {
        let mut encoder = zstd::Encoder::new(Vec::new(), self.level)?;
        encoder.multithread(num_cpus::get() as u32)?;
        encoder.write_all(&self.data)?;
        self.file.write_all(&encoder.finish()?)?;
        Ok(())
    }

    /// Compresses the data into per-CPU frames plus a seek table, the version 2 layout.
    fn finish_v2(&mut self) -> io::Result<()> {
        let ranges = self.frame_ranges();
        // Frames are independent, so compress them in parallel; par_iter preserves order.
        let frames: Vec<Vec<u8>> = ranges
            .par_iter()
            .map(|&(start, end)| zstd::stream::encode_all(&self.data[start..end], self.level))
            .collect::<io::Result<_>>()?;

        let mut frame_lens = Vec::with_capacity(frames.len());
        for frame in &frames {
            self.file.write_all(frame)?;
            frame_lens.push(frame.len() as u32);
        }
        self.write_seek_table(&frame_lens)
    }

    /// Finish the encoding and return the size in bytes of the compressed file that was created.
    pub fn finish(mut self) -> io::Result<u64> {
        if self.version == 1 {
            self.finish_v1()?;
        } else {
            self.finish_v2()?;
        }
        let mut file = self.file.into_inner()?;
        file.stream_position()
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("expected file to start with nix-index file magic 'NIXI', but found '{found:?}' (is this a valid nix-index database file?)")]
    UnsupportedFileType { found: Vec<u8> },
    #[error("this executable only supports nix-index database versions 1 and 2, but found version {found}")]
    UnsupportedVersion { found: u64 },
    #[error("database corrupt, found a file entry without a matching package entry")]
    MissingPackageEntry,
    #[error("database corrupt, could not parse the seek table")]
    CorruptSeekTable,
    #[error("database corrupt, frcode error: {0}")]
    Frcode(#[from] frcode::Error),
    #[error("database corrupt, could not parse entry: {entry:?}")]
    EntryParse { entry: Vec<u8> },
    #[error("database corrupt, could not parse store path: {path:?}")]
    StorePathParse { path: Vec<u8> },
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("grep error: {0}")]
    Grep(#[from] grep::regex::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// A Reader allows fast querying of a nix-index database.
pub struct Reader {
    /// The entire database file, kept in memory so frames can be sliced and decompressed
    /// in parallel during a query.
    data: Vec<u8>,
    /// Byte range `(offset, len)` of every compressed frame within `data`.
    frames: Vec<(usize, usize)>,
}

impl Reader {
    /// Opens a nix-index database located at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Reader> {
        let data = std::fs::read(path)?;

        if data.len() < DATA_START || data[..FILE_MAGIC.len()] != *FILE_MAGIC {
            return Err(Error::UnsupportedFileType {
                found: data[..FILE_MAGIC.len().min(data.len())].to_vec(),
            });
        }

        let version = (&data[FILE_MAGIC.len()..DATA_START])
            .read_u64::<LittleEndian>()
            .expect("slice has 8 bytes");
        let frames = match version {
            // v1 is a single zstd frame spanning the rest of the file; v2 stores per-frame
            // offsets in a seek table.
            1 => vec![(DATA_START, data.len() - DATA_START)],
            2 => Self::parse_seek_table(&data)?,
            _ => return Err(Error::UnsupportedVersion { found: version }),
        };
        Ok(Reader { data, frames })
    }

    /// Reads a little-endian u32 at `pos`, erroring on truncation.
    fn read_u32_at(data: &[u8], pos: usize) -> Result<u32> {
        let end = pos + 4;
        if end > data.len() {
            return Err(Error::CorruptSeekTable);
        }
        Ok(u32::from_le_bytes(
            data[pos..end].try_into().expect("range is exactly 4 bytes"),
        ))
    }

    /// Parses the trailing seek table into the byte range of each data frame.
    fn parse_seek_table(data: &[u8]) -> Result<Vec<(usize, usize)>> {
        let len = data.len();
        // The skippable frame is `[magic u32][size u32][payload]`; the payload ends with its
        // own length again so we can locate the frame header from the end of the file.
        let payload_len = Self::read_u32_at(data, len - 4)? as usize;
        if payload_len + 8 > len {
            return Err(Error::CorruptSeekTable);
        }
        let frame_start = len - 8 - payload_len;
        if Self::read_u32_at(data, frame_start)? != SKIPPABLE_MAGIC {
            return Err(Error::CorruptSeekTable);
        }

        let mut pos = frame_start + 8;
        let count = Self::read_u32_at(data, pos)? as usize;
        pos += 4;
        let mut frames = Vec::with_capacity(count);
        let mut offset = DATA_START;
        for _ in 0..count {
            let flen = Self::read_u32_at(data, pos)? as usize;
            pos += 4;
            frames.push((offset, flen));
            offset += flen;
        }
        Ok(frames)
    }

    /// Builds a query to find all entries whose filename matches the given pattern. Use
    /// `Query::run` to iterate over the items.
    pub fn query(self, exact_regex: &Regex) -> Query<'_, '_> {
        Query {
            reader: self,
            exact_regex,
            hash: None,
            package_pattern: None,
        }
    }

    /// Dumps the contents of the database to stdout, for debugging.
    #[allow(clippy::print_stdout)]
    pub fn dump(&mut self) -> Result<()> {
        for &(offset, flen) in &self.frames {
            let raw = zstd::stream::decode_all(&self.data[offset..offset + flen])?;
            let mut decoder = frcode::Decoder::new(io::Cursor::new(&raw[..]));
            loop {
                let block = decoder.decode()?;
                if block.is_empty() {
                    break;
                }
                for line in block.split(|c| *c == b'\n') {
                    println!("{:?}", String::from_utf8_lossy(line));
                }
                println!("-- block boundary");
            }
            println!("-- frame boundary");
        }
        Ok(())
    }
}

/// A builder for a `ReaderIter` to iterate over entries in the database matching a given pattern.
pub struct Query<'a, 'b> {
    /// The underlying reader from which we read input.
    reader: Reader,

    /// The pattern that file paths have to match.
    exact_regex: &'a Regex,

    /// Only include the package with the given hash.
    hash: Option<String>,

    /// Only include packages whose name matches the given pattern.
    package_pattern: Option<&'b Regex>,
}

impl<'a, 'b> Query<'a, 'b> {
    /// Limit results to entries from the package with the specified hash if `Some`.
    pub fn hash(self, hash: Option<String>) -> Query<'a, 'b> {
        Query { hash, ..self }
    }

    /// Limit results to entries from packages whose name matches the given regex if `Some`.
    pub fn package_pattern(self, package_pattern: Option<&'b Regex>) -> Query<'a, 'b> {
        Query {
            package_pattern,
            ..self
        }
    }

    /// Runs the query, returning an Iterator that will yield all entries matching the conditions.
    ///
    /// There is no guarantee about the order of the returned matches.
    pub fn run(self) -> Result<QueryResults> {
        let mut expr = regex_syntax::ast::parse::Parser::new()
            .parse(self.exact_regex.as_str())
            .expect("regex cannot be invalid");
        // replace the ^ anchor by a NUL byte, since each entry is of the form `METADATA\0PATH`
        // (so the NUL byte marks the start of the path).
        {
            let mut stack = vec![&mut expr];
            while let Some(e) = stack.pop() {
                match e {
                    Ast::Assertion(a) if a.kind == AssertionKind::StartLine => {
                        *e = Ast::Literal(Box::new(Literal {
                            span: a.span,
                            c: '\0',
                            kind: regex_syntax::ast::LiteralKind::Verbatim,
                        }))
                    }
                    Ast::Group(g) => stack.push(&mut g.ast),
                    Ast::Repetition(r) => stack.push(&mut r.ast),
                    Ast::Concat(c) => stack.extend(c.asts.iter_mut()),
                    Ast::Alternation(a) => stack.extend(a.asts.iter_mut()),
                    _ => {}
                }
            }
        }
        let mut regex_builder = grep::regex::RegexMatcherBuilder::new();
        regex_builder.line_terminator(Some(b'\n')).multi_line(true);

        let grep = regex_builder.build(&format!("{}", expr))?;
        let matchers = Matchers {
            pattern: grep,
            exact_pattern: self.exact_regex,
            package_entry_pattern: regex_builder.build("^p\0").expect("valid regex"),
            package_name_pattern: self.package_pattern,
            package_hash: self.hash.as_deref(),
        };

        let reader = self.reader;
        // Frames are independent, so search them in parallel and concatenate the results.
        let per_frame: Vec<Vec<(StorePath, FileTreeEntry)>> = reader
            .frames
            .par_iter()
            .map(|&(offset, flen)| search_frame(&reader.data[offset..offset + flen], &matchers))
            .collect::<Result<_>>()?;

        let all: Vec<_> = per_frame.into_iter().flatten().collect();
        Ok(QueryResults {
            inner: all.into_iter(),
        })
    }
}

/// The compiled patterns and package constraints for a query, shared by reference across all
/// frames searched in parallel.
struct Matchers<'a, 'b> {
    /// Runs on the raw bytes of a file entry. Since the path is not the first field, the `^`
    /// anchor won't work here, and this may match inside metadata; matches are therefore
    /// re-checked against `exact_pattern`.
    pattern: grep::regex::RegexMatcher,
    /// The real path pattern, used to reject the false positives `pattern` may produce.
    exact_pattern: &'a Regex,
    /// Pattern that matches only package entries.
    package_entry_pattern: grep::regex::RegexMatcher,
    /// Pattern that the package name should match.
    package_name_pattern: Option<&'b Regex>,
    /// Only search the package with the given hash.
    package_hash: Option<&'b str>,
}

/// An iterator over the entries in a database that matched a query.
pub struct QueryResults {
    inner: std::vec::IntoIter<(StorePath, FileTreeEntry)>,
}

fn consume_no_error<T>(e: NoError) -> T {
    panic!("impossible: {}", e)
}

fn next_matching_line<M: Matcher<Error = NoError>>(
    matcher: M,
    buf: &[u8],
    mut start: usize,
) -> Option<Match> {
    while let Some(candidate) = matcher
        .find_candidate_line(&buf[start..])
        .unwrap_or_else(consume_no_error)
    {
        // the buffer may end with a newline character, so we may get a match
        // for an empty "line" at the end of the buffer
        // since this is not a line match, return None
        if start == buf.len() {
            return None;
        };

        let (pos, confirmed) = match candidate {
            LineMatchKind::Confirmed(pos) => (start + pos, true),
            LineMatchKind::Candidate(pos) => (start + pos, false),
        };

        let line_start = memrchr(b'\n', &buf[..pos]).map_or(0, |x| x + 1);
        let line_end = memchr(b'\n', &buf[pos..]).map_or(buf.len(), |x| x + pos + 1);

        if !confirmed
            && !matcher
                .is_match(&buf[line_start..line_end])
                .unwrap_or_else(consume_no_error)
        {
            start = line_end;
            continue;
        }

        return Some(Match::new(line_start, line_end));
    }
    None
}

/// Decompresses a single database frame and returns all entries in it that match `m`.
///
/// A frame is self-contained: it starts a fresh frcode stream and always ends at a package
/// boundary, so the package entry for every match is guaranteed to appear within the frame.
fn search_frame(compressed: &[u8], m: &Matchers) -> Result<Vec<(StorePath, FileTreeEntry)>> {
    let raw = zstd::stream::decode_all(compressed)?;
    let mut decoder = frcode::Decoder::new(io::Cursor::new(&raw[..]));
    let mut found: Vec<(StorePath, FileTreeEntry)> = Vec::new();
    let mut found_without_package: Vec<FileTreeEntry> = Vec::new();

    loop {
        let block = decoder.decode()?;
        if block.is_empty() {
            break;
        }

        // A match tells us the file entry, but the package it belongs to is stored *after* all
        // its file entries. find_package skips forward to that package entry and caches it,
        // since consecutive matches usually share the same package.
        let mut cached_package: Option<(StorePath, usize)> = None;
        let mut no_more_package = false;
        let mut find_package = |item_end| -> Result<_> {
            if let Some((ref pkg, end)) = cached_package {
                if item_end < end {
                    return Ok(Some((pkg.clone(), end)));
                }
            }
            if no_more_package {
                return Ok(None);
            }

            let mat = match next_matching_line(&m.package_entry_pattern, block, item_end) {
                Some(v) => v,
                None => {
                    no_more_package = true;
                    return Ok(None);
                }
            };

            let json = &block[mat.start() + 2..mat.end() - 1];
            let pkg: StorePath =
                serde_json::from_slice(json).map_err(|_| Error::StorePathParse {
                    path: json.to_vec(),
                })?;
            cached_package = Some((pkg.clone(), mat.end()));
            Ok(Some((pkg, mat.end())))
        };

        let should_search_package = |pkg: &StorePath| -> bool {
            m.package_name_pattern
                .is_none_or(|r| r.is_match(pkg.name().as_bytes()))
                && m.package_hash.is_none_or(|h| pkg.hash().as_ref() == h)
        };

        let mut pos = 0;
        // Resolve matches carried over from the previous block, whose package entry may only
        // now be in reach.
        if !found_without_package.is_empty() {
            if let Some((pkg, end)) = find_package(0)? {
                if !should_search_package(&pkg) {
                    pos = end;
                    found_without_package.truncate(0);
                } else {
                    for entry in found_without_package.split_off(0) {
                        found.push((pkg.clone(), entry));
                    }
                }
            }
        }

        while let Some(mat) = next_matching_line(&m.pattern, block, pos) {
            pos = mat.end();
            let entry = &block[mat.start()..mat.end() - 1];
            // skip entries that aren't describing file paths
            if m.package_entry_pattern
                .is_match(entry)
                .unwrap_or_else(consume_no_error)
            {
                continue;
            }

            // skip if the package name or hash doesn't match; only possible once we know it
            if let Some((pkg, end)) = find_package(mat.end())? {
                if !should_search_package(&pkg) {
                    pos = end;
                    continue;
                }
            }

            let entry = FileTreeEntry::decode(entry).ok_or_else(|| Error::EntryParse {
                entry: entry.to_vec(),
            })?;

            // pattern may match inside metadata, so verify against the real path
            if !m.exact_pattern.is_match(&entry.path) {
                continue;
            }

            match find_package(mat.end())? {
                None => found_without_package.push(entry),
                Some((pkg, _)) => found.push((pkg, entry)),
            }
        }
    }

    Ok(found)
}

impl Iterator for QueryResults {
    type Item = Result<(StorePath, FileTreeEntry)>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_matching_line_package() {
        let matcher = grep::regex::RegexMatcherBuilder::new()
            .line_terminator(Some(b'\n'))
            .multi_line(true)
            .build("^p")
            .expect("valid regex");
        let buffer = br#"
SOME LINE
pDATA
ANOTHER LINE
        "#;

        let mat = next_matching_line(matcher, buffer, 0);
        assert_eq!(mat, Some(Match::new(11, 17)));
    }

    /// Writes many packages (enough to span several independently-compressed frames) and
    /// verifies every entry can be found again after the multi-frame round trip.
    #[test]
    fn test_multi_frame_round_trip() {
        use std::collections::HashMap;

        use serde_bytes::ByteBuf;

        use crate::files::FileTree;
        use crate::package::PathOrigin;

        let dir = std::env::temp_dir().join(format!("nix-index-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("files");

        // Enough packages that the total uncompressed size clears several FRAME_BUDGETs.
        let num_packages = 4000;
        let mut writer = Writer::create(&db_path, 3, 2).unwrap();
        for i in 0..num_packages {
            let origin = PathOrigin {
                attr: format!("pkg{i}"),
                output: "out".to_string(),
                toplevel: true,
                system: None,
            };
            let path =
                StorePath::parse(origin, &format!("/nix/store/{:032x}-pkg{i}-1.0", i as u128))
                    .unwrap();

            let mut bin = HashMap::new();
            bin.insert(
                ByteBuf::from(format!("prog{i}").into_bytes()),
                FileTree::regular(100 + i as u64, true),
            );
            let mut root = HashMap::new();
            root.insert(ByteBuf::from(b"bin".to_vec()), FileTree::directory(bin));
            writer.add(path, FileTree::directory(root), b"").unwrap();
        }
        let size = writer.finish().unwrap();
        assert!(size > 0);

        // Every package's unique binary must be found exactly once.
        for i in [0usize, 1, 1234, num_packages - 1] {
            let reader = Reader::open(&db_path).unwrap();
            let pattern = Regex::new(&format!("bin/prog{i}$")).unwrap();
            let results: Vec<_> = reader
                .query(&pattern)
                .run()
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            assert_eq!(results.len(), 1, "prog{i} should be found once");
            let (store_path, entry) = &results[0];
            assert_eq!(store_path.name(), format!("pkg{i}-1.0"));
            assert_eq!(entry.path, format!("/bin/prog{i}").into_bytes());
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Writing and reading back the legacy version 1 (single-frame) format.
    #[test]
    fn test_v1_write_read() {
        use std::collections::HashMap;

        use serde_bytes::ByteBuf;

        use crate::files::FileTree;
        use crate::package::PathOrigin;

        let dir = std::env::temp_dir().join(format!("nix-index-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("files");

        let origin = PathOrigin {
            attr: "hello".to_string(),
            output: "out".to_string(),
            toplevel: true,
            system: None,
        };
        let path = StorePath::parse(
            origin,
            "/nix/store/00000000000000000000000000000000-hello-1.0",
        )
        .unwrap();
        let mut bin = HashMap::new();
        bin.insert(ByteBuf::from(b"hello".to_vec()), FileTree::regular(1, true));
        let mut root = HashMap::new();
        root.insert(ByteBuf::from(b"bin".to_vec()), FileTree::directory(bin));

        let mut writer = Writer::create(&db_path, 3, 1).unwrap();
        writer.add(path, FileTree::directory(root), b"").unwrap();
        writer.finish().unwrap();

        // Header must record version 1.
        let bytes = std::fs::read(&db_path).unwrap();
        assert_eq!(&bytes[..FILE_MAGIC.len()], FILE_MAGIC);
        assert_eq!(
            u64::from_le_bytes(bytes[FILE_MAGIC.len()..DATA_START].try_into().unwrap()),
            1
        );

        let reader = Reader::open(&db_path).unwrap();
        assert_eq!(reader.frames.len(), 1);
        let results: Vec<_> = reader
            .query(&Regex::new("bin/hello$").unwrap())
            .run()
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.path, b"/bin/hello");

        std::fs::remove_dir_all(&dir).ok();
    }
}
