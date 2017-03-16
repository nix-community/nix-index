use std::io::{self, Write, BufRead};
use std::fmt;

pub enum Error {
    SharedOutOfRange {
        previous_len: usize,
        shared_len: isize,
    },
    SharedOverflow {
        shared_len: isize,
        diff: isize,
    },
    MissingNul,
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error { Error::Io(err) }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::Error::*;
        match self {
            &Io(ref e) => write!(f, "{}", e),
            &SharedOutOfRange { previous_len, shared_len } =>
                write!(f, "length of shared prefix out of bounds [0, {}): {}", previous_len, shared_len),
            &SharedOverflow { shared_len, diff } =>
                write!(f, "cannot add {} to shared prefix length {}: overflow", diff, shared_len),
            &MissingNul =>
                write!(f, "entry missing terminal NUL byte"),
        }
    }
}

pub struct Decoder<R> {
    reader: R,
    entry: Vec<u8>,
    shared_len: isize,
}

pub enum Item<'a> {
    Entry(&'a [u8]),
    Footer(Vec<u8>),
    EOF,
}

impl<R: BufRead> Decoder<R> {
    pub fn new(reader: R) -> Decoder<R> {
        Decoder {
            reader: reader,
            entry: Vec::new(),
            shared_len: 0,
        }
    }

    pub fn decode<'a>(&'a mut self) -> Result<Item<'a>, Error> {
        let diff = {
            let mut buf = [0; 1];
            if self.reader.read(&mut buf)? == 0 {
                return Ok(Item::EOF)
            }

            if buf[0] != 0x80 {
                (buf[0] as i8) as i16
            } else {
                let mut buf = [0; 2];
                self.reader.read_exact(&mut buf)?;
                let high = buf[0] as i16;
                let low = buf[1] as i16;
                high << 8 | low
            }
        } as isize;

        self.shared_len = match self.shared_len.checked_add(diff) {
            Some(v) => v,
            None => return Err(Error::SharedOverflow {
                shared_len: self.shared_len,
                diff: diff,
            })
        };
        if self.shared_len < 0 || self.shared_len as usize > self.entry.len() {
            return Err(Error::SharedOutOfRange {
                previous_len: self.entry.len(),
                shared_len: self.shared_len,
            })
        }

        self.entry.resize(self.shared_len as usize, 0);
        self.reader.read_until(b'\0', &mut self.entry)?;

        if self.entry.pop() != Some(b'\x00') {
            return Err(Error::MissingNul)
        }

        if self.entry.get(0) != Some(&b'\x01') {
            Ok(Item::Entry(&self.entry))
        } else {
            Ok(Item::Footer(self.entry.split_off(1)))
        }

    }
}

pub struct Encoder<W: Write> {
    writer: W,
    entry: Vec<u8>,
    shared_len: i16,
    footer: Vec<u8>,
    footer_written: bool,
}

impl<W: Write> Drop for Encoder<W> {
    fn drop(&mut self) {
        self.write_footer().expect("failed to write footer")
    }
}

impl<W: Write> Encoder<W> {
    pub fn new(writer: W, footer: Vec<u8>) -> Encoder<W> {
        assert!(!footer.contains(&b'\x00'), "footer must not contain null bytes");
        Encoder {
            writer: writer,
            entry: Vec::new(),
            shared_len: 0,
            footer: footer,
            footer_written: false,
        }
    }

    fn encode_diff(&mut self, diff: i16) -> io::Result<()> {
        let low = (diff & 0xFF) as u8;
        if diff.abs() < i8::max_value() as i16 {
            self.writer.write_all(&[low])?;
        } else {
            let high = ((diff >> 8) & 0xFF) as u8;
            self.writer.write_all(&[0x80, high, low])?;
        }
        Ok(())
    }

    pub fn encode<'a>(&'a mut self, data: Vec<u8>) -> io::Result<&'a [u8]> {
        assert!(!data.contains(&b'\x00'), "entry must not contain null bytes");
        assert!(data.get(0) != Some(&b'\x01'), "entry must not start with 0x01");

        let mut shared: isize = 0;
        let max_shared = i16::max_value() as isize;
        for (a, b) in self.entry.iter().zip(data.iter()) {
            if a != b || shared > max_shared { break }
            shared += 1;
        }
        let shared = shared as i16;

        let diff = shared - self.shared_len;
        self.encode_diff(diff)?;

        self.entry = data;
        self.shared_len = shared;

        let pos = shared as usize;
        if pos < self.entry.len() {
            self.writer.write_all(&self.entry[pos..])?;
        }
        self.writer.write_all(b"\0")?;

        Ok(&self.entry)
    }

    fn write_footer<'a>(&mut self) -> io::Result<()> {
        if self.footer_written {
            return Ok(())
        }

        let diff = - self.shared_len;
        self.encode_diff(diff)?;
        self.writer.write_all(b"\x01")?;
        self.writer.write_all(&self.footer)?;
        self.writer.write_all(b"\x00")?;

        self.footer_written = true;

        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.write_footer()?;

        Ok(())
    }
}
