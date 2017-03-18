use std::io::{self, Read, Write, BufWriter, BufReader, Seek, SeekFrom};
use std::fs::{File};
use std::path::{Path};
use std::fmt;
use zstd;
use grep::{Grep, Match};
use memchr::memchr;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

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
        let mut encoder = frcode::Encoder::new(writer, path.encode()?);
        for entry in files.to_list() {
            let mut encoded = Vec::new();
            entry.encode(&mut encoded)?;
            encoder.encode(encoded)?;
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
    EntryParseFailed(Vec<u8>),
    StorePathParseFailed(Vec<u8>),
}

const FORMAT_VERSION: u64 = 1;
const FILE_MAGIC: &'static [u8] = b"NIXI";

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::Error::*;
        match self {
            &Io(ref e) => write!(f, "i/o error: {}", e),
            &Frcode(ref e) => write!(f, "frcode format error: {}", e),
            &MissingFileMeta(ref e) => write!(f, "format error, file without meta information: {}", String::from_utf8_lossy(e)),
            &EntryParseFailed(ref e) => write!(f, "failed to parse entry. raw entry: {}", String::from_utf8_lossy(e)),
            &StorePathParseFailed(ref e) => write!(f, "failed to parse store path. raw bytes: {}", String::from_utf8_lossy(e)),
            &UnsupportedVersion(v) => write!(f, "this executable only supports the nix-index database version {}, but found a database with version {}", FORMAT_VERSION, v),
            &UnsupportedFileType => write!(f, "the file is not a nix-index database")
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
    buf: Vec<u8>,
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
            buf: Vec::new(),
        })
    }

    pub fn find_iter<'a, 'b>(&'a mut self, pattern: &'b Grep) -> ReaderIter<'a, 'b> {
        ReaderIter {
            reader: self,
            pos: 0,
            package: Vec::new(),
            pattern: pattern,
        }
    }
}

pub struct ReaderIter<'a, 'b> {
    reader: &'a mut Reader,
    pos: usize,
    package: Vec<u8>,
    pattern: &'b Grep,
}

impl<'a, 'b> ReaderIter<'a, 'b> {
    fn fill_package(&mut self) -> Result<bool, Error> {
        self.reader.buf.clear();
        self.pos = 0;
        loop {
            match self.reader.decoder.decode()? {
                frcode::Item::Entry(path) => {
                    self.reader.buf.extend_from_slice(path);
                    self.reader.buf.push(b'\n');
                },
                frcode::Item::Footer(info) => {
                    self.package = info;
                    self.pos = 0;
                    return Ok(true);
                },
                frcode::Item::EOF => {
                    return Ok(false)
                }
            }
        }
    }

    pub fn next_match(&mut self) -> Result<Option<(StorePath, FileTreeEntry)>, Error> {
        let mut mat = Match::new();
        loop {
            let found = self.pattern.read_match(&mut mat, &self.reader.buf, self.pos);

            if !found {
                if self.fill_package()? {
                    continue
                } else {
                    return Ok(None)
                }
            }

            if self.reader.buf[mat.start()] != b'/' {
                self.pos = mat.end();
                continue
            }

            break
        }

        self.pos = mat.end() + memchr(b'\n', &self.reader.buf[mat.end()..]).ok_or_else(|| {
            let file = &self.reader.buf[mat.start()..mat.end()];
            Error::MissingFileMeta(file.to_vec())
        })?;

        let store_path = StorePath::decode(&self.package).ok_or_else(|| {
            Error::StorePathParseFailed(self.package.clone())
        })?;

        let entry = &self.reader.buf[mat.start()..self.pos];
        let entry = FileTreeEntry::decode(entry).ok_or_else(|| {
            Error::EntryParseFailed(entry.to_vec())
        })?;

        // skip over the newline character
        self.pos += 1;

        Ok(Some((store_path, entry)))
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
