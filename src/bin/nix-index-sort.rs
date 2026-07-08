use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::PathBuf,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use clap::Parser;
use nix_index::database::{FILE_MAGIC, FORMAT_VERSION};

#[derive(Debug, PartialEq, PartialOrd, Eq, Ord)]
struct Package {
    meta: Vec<u8>,

    paths: Vec<Vec<u8>>,
}

struct PackageStream<R> {
    reader: R,

    buffer: Vec<u8>,
    pending_entries: Vec<Vec<u8>>,
}

impl<R> PackageStream<R> {
    fn new(reader: R) -> Self {
        PackageStream {
            reader,
            buffer: Vec::new(),
            pending_entries: Vec::new(),
        }
    }
}

impl<R: BufRead> Iterator for PackageStream<R> {
    type Item = Result<Package, std::io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let &mut PackageStream {
            ref mut reader,
            ref mut buffer,
            ref mut pending_entries,
            ..
        } = self;
        loop {
            buffer.clear();
            let len = match reader.read_until(b'\n', buffer) {
                Ok(len) => len,
                Err(err) => return Some(Err(err)),
            };
            if len == 0 {
                return None;
            }
            if buffer.starts_with(b"p\0") {
                return Some(Ok(Package {
                    paths: std::mem::take(pending_entries),
                    meta: buffer[..len].into(),
                }));
            } else {
                pending_entries.push(buffer[..len].into());
            }
        }
    }
}

// Sorts the index database.
//
// This makes the database reproducible, meaning that building it at a given nixpkgs commit
// produces the same file byte for byte.
#[derive(Debug, Parser)]
#[clap(author, about, version)]
struct Args {
    /// Directory where the index is stored
    #[clap(short, long = "input-db", env = "NIX_INDEX_DATABASE")]
    input_database: PathBuf,

    #[clap(short, long = "output-db")]
    output_database: PathBuf,

    /// Zstandard compression level
    #[clap(short, long = "compression", default_value = "22")]
    compression_level: i32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut file = File::open(args.input_database).unwrap();
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    let _version = file.read_u64::<LittleEndian>()?;

    let reader = BufReader::new(zstd::Decoder::new(file).unwrap());

    let packages = PackageStream::new(reader);

    // TODO: use extsort instead of an in-memory sort.
    let mut res = packages.collect::<Result<Vec<_>, std::io::Error>>()?;
    res.sort();
    let mut file = File::create(args.output_database)?;
    file.write_all(FILE_MAGIC)?;
    file.write_u64::<LittleEndian>(FORMAT_VERSION)?;
    let mut encoder = zstd::Encoder::new(file, args.compression_level)?;
    encoder.multithread(num_cpus::get() as u32)?;
    let mut writer = BufWriter::new(encoder);
    for package in res {
        for path in &package.paths {
            writer.write_all(path)?;
        }
        writer.write_all(&package.meta)?;
        // for path in &package.paths {
        //     println!("{:?}", String::from_utf8_lossy(path));
        // }
        // println!("{:?}", String::from_utf8_lossy(&package.meta));
    }
    if let Ok(enc) = writer.into_inner() {
        enc.finish()?;
    }
    Ok(())
}
