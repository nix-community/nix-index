use std::io::{self, Read, Write, BufWriter, BufReader, Seek, SeekFrom};
use std::fs::{File};
use std::path::{Path};
use std::fmt;
use zstd;
use grep::{Grep, Match, GrepBuilder};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde_json;

use package::{StorePath};
use files::{FileTree, FileTreeEntry};
use frcode;

pub struct Writer {
    writer: Option<BufWriter<zstd::Encoder<File>>>,
}

impl Drop for Writer {
    fn drop(&mut self) {
        if self.writer.is_some() {
            self.finish_encoder().unwrap();
        }
    }
}

impl Writer {
    pub fn create<P: AsRef<Path>>(path: P, level: i32) -> io::Result<Writer> {
        let mut file = File::create(path)?;
        file.write_all(FILE_MAGIC)?;
        file.write_u64::<LittleEndian>(FORMAT_VERSION)?;
        let encoder = zstd::Encoder::new(file, level)?;

        Ok(Writer {
            writer: Some(BufWriter::new(encoder))
        })
    }

    pub fn add(&mut self, path: StorePath, files: FileTree) -> io::Result<()> {
        let writer = self.writer.as_mut().expect("not dropped yet");
        let mut encoder = frcode::Encoder::new(writer, b"p".to_vec(), serde_json::to_vec(&path).unwrap());
        for entry in files.to_list() {
            entry.encode(&mut encoder)?;
        }
        Ok(())
    }

    fn finish_encoder(&mut self) -> io::Result<File> {
        let writer = self.writer.take().expect("not dropped yet");
        let encoder = writer.into_inner()?;
        encoder.finish()
    }

    pub fn finish(mut self) -> io::Result<u64> {
        let mut file = self.finish_encoder()?;
        file.seek(SeekFrom::Current(0))
    }
}


pub enum Error {
    Io(io::Error),
    Frcode(frcode::Error),
    UnsupportedFileType,
    UnsupportedVersion(u64),
    MissingFileMeta(Vec<u8>),
    MissingPackageEntry,
    EntryParseFailed(Vec<u8>),
    StorePathParseFailed(Vec<u8>),
}

const FORMAT_VERSION: u64 = 1;
const FILE_MAGIC: &'static [u8] = b"NIXI";

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::Error::*;
        match *self {
            Io(ref e) => write!(f, "i/o error: {}", e),
            Frcode(ref e) => write!(f, "frcode format error: {}", e),
            MissingFileMeta(ref e) => write!(f, "format error, file without meta information: {}", String::from_utf8_lossy(e)),
            EntryParseFailed(ref e) => write!(f, "failed to parse entry. raw entry: {}", String::from_utf8_lossy(e)),
            StorePathParseFailed(ref e) => write!(f, "failed to parse store path. raw bytes: {}", String::from_utf8_lossy(e)),
            MissingPackageEntry => write!(f, "format error, found a file entry without matching package entry"),
            UnsupportedVersion(v) => write!(f, "this executable only supports the nix-index database version {}, but found a database with version {}", FORMAT_VERSION, v),
            UnsupportedFileType => write!(f, "the file is not a nix-index database")
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self { Error::Io(err) }
}

impl From<frcode::Error> for Error {
    fn from(err: frcode::Error) -> Self { Error::Frcode(err) }
}

pub struct Reader {
    decoder: frcode::Decoder<BufReader<zstd::Decoder<File>>>,
}

impl Reader {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Reader, Error> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;

        if magic != FILE_MAGIC {
            return Err(Error::UnsupportedFileType)
        }

        let version = file.read_u64::<LittleEndian>()?;
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version))
        }

        let decoder = zstd::Decoder::new(file)?;
        Ok(Reader {
            decoder: frcode::Decoder::new(BufReader::new(decoder)),
        })
    }

    pub fn find_iter<'a, 'b>(&'a mut self, pattern: &'b Grep) -> ReaderIter<'a, 'b> {
        ReaderIter {
            reader: self,
            found: Vec::new(),
            found_without_package: Vec::new(),
            pattern: pattern,
            package_entry_pattern: GrepBuilder::new("^p\0").build().expect("valid regex"),
        }
    }
}

pub struct ReaderIter<'a, 'b> {
    reader: &'a mut Reader,
    found: Vec<(StorePath, FileTreeEntry)>,
    found_without_package: Vec<FileTreeEntry>,
    pattern: &'b Grep,
    package_entry_pattern: Grep, 
}

impl<'a, 'b> ReaderIter<'a, 'b> {
    fn fill_buf(&mut self) -> Result<(), Error> {
        while self.found.is_empty() {
            let &mut ReaderIter {
                ref mut reader,
                ref package_entry_pattern,
                ..
            } = self;
            let block = reader.decoder.decode()?;

            if block.is_empty() {
                return Ok(())
            }

            let mut cached_package: Option<(StorePath, usize)> = None;
            let mut no_more_package = false;
            let mut find_package = |item_end| -> Result<_, Error> {
                if let Some((ref pkg, end)) = cached_package {
                    if item_end < end {
                        return Ok(Some(pkg.clone()))
                    }
                }

                let mut mat = Match::new();
                if no_more_package || !package_entry_pattern.read_match(&mut mat, block, item_end) {
                    no_more_package = true;
                    return Ok(None)
                }

                let json = &block[mat.start() + 2..mat.end()-1];
                let pkg: StorePath = serde_json::from_slice(json).ok().ok_or_else(|| {
                    Error::StorePathParseFailed(json.to_vec())
                })?;
                cached_package = Some((pkg.clone(), mat.end()));
                Ok(Some(pkg))
            };

            if !self.found_without_package.is_empty() {
                if let Some(pkg) = find_package(0)? {
                    for entry in self.found_without_package.split_off(0) {
                        self.found.push((pkg.clone(), entry));
                    }
                }
            }

            for mat in self.pattern.iter(block) {
                let entry = &block[mat.start()..mat.end()-1];
                if self.package_entry_pattern.regex().is_match(entry) {
                    continue
                }
                let entry = FileTreeEntry::decode(entry).ok_or_else(|| {
                    Error::EntryParseFailed(entry.to_vec())
                })?;

                match find_package(mat.end())? {
                    None => self.found_without_package.push(entry),
                    Some(pkg) => self.found.push((pkg, entry))
                }
            }
        }
        Ok(())
    }
    pub fn next_match(&mut self) -> Result<Option<(StorePath, FileTreeEntry)>, Error> {
        self.fill_buf()?;
        Ok(self.found.pop())
    }
}

impl<'a, 'b> Iterator for ReaderIter<'a, 'b> {
    type Item = Result<(StorePath, FileTreeEntry), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_match() {
            Err(e) => Some(Err(e)),
            Ok(v) => v.map(Ok),
        }
    }
}
