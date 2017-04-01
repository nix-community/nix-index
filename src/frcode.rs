use std::io::{self, Write, BufRead};
use std::fmt;
use std::cmp;
use std::ops::{Deref, DerefMut};
use memchr;

pub enum Error {
    SharedOutOfRange {
        previous_len: usize,
        shared_len: isize,
    },
    SharedOverflow { shared_len: isize, diff: isize },
    MissingNul,
    MissingNewline,
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        Error::Io(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::Error::*;
        match *self {
            Io(ref e) => write!(f, "{}", e),
            SharedOutOfRange {
                previous_len,
                shared_len,
            } => {
                write!(f,
                       "length of shared prefix out of bounds [0, {}): {}",
                       previous_len,
                       shared_len)
            }
            SharedOverflow { shared_len, diff } => {
                write!(f,
                       "cannot add {} to shared prefix length {}: overflow",
                       diff,
                       shared_len)
            }
            MissingNul => write!(f, "entry missing terminal NUL byte"),
            MissingNewline => write!(f, "entry missing terminating newline"),
        }
    }
}

struct ResizableBuf {
    allow_resize: bool,
    data: Vec<u8>,
}

impl ResizableBuf {
    fn new(capacity: usize) -> ResizableBuf {
        ResizableBuf {
            data: vec![0; capacity],
            allow_resize: true,
        }
    }
    fn resize(&mut self, new_size: usize) -> bool {
        if new_size <= self.data.len() {
            return true;
        }

        if !self.allow_resize {
            return false;
        }

        self.data.resize(new_size, b'\0');
        true
    }
}

impl Deref for ResizableBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl DerefMut for ResizableBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

pub struct Decoder<R> {
    eof: bool,
    reader: R,
    last_path: usize,
    partial_item_start: usize,
    shared_len: isize,
    buf: ResizableBuf,
    pos: usize,
}

pub struct Item {
    pub meta: Vec<u8>,
    pub kind: u8,
    pub path: Vec<u8>,
}

impl<R: BufRead> Decoder<R> {
    pub fn new(reader: R) -> Decoder<R> {
        let capacity = 16_000;
        Decoder {
            reader: reader,
            buf: ResizableBuf::new(capacity),
            pos: 0,
            last_path: 0,
            shared_len: 0,
            partial_item_start: 0,
            eof: false,
        }
    }

    fn copy_shared(&mut self) -> Result<bool, Error> {
        let shared_len = self.shared_len as usize;
        let new_pos = self.pos + shared_len;
        let new_last_path = self.pos;
        if !self.buf.resize(new_pos) {
            return Ok(false);
        }


        if self.shared_len < 0 || self.last_path + shared_len > self.pos {
            return Err(Error::SharedOutOfRange {
                           previous_len: self.pos - self.last_path,
                           shared_len: self.shared_len,
                       });
        }

        let (_, last) = self.buf.split_at_mut(self.last_path);
        let (last, new) = last.split_at_mut(self.pos - self.last_path);
        new[..shared_len].copy_from_slice(&last[..shared_len]);

        self.pos += shared_len;
        self.last_path = new_last_path;
        Ok(true)
    }

    fn read_to_nul(&mut self) -> Result<bool, Error> {
        loop {
            let (done, len) = {
                let &mut Decoder {
                             ref mut reader,
                             ref mut buf,
                             ref mut pos,
                             ..
                         } = self;
                let input = match reader.fill_buf() {
                    Ok(data) => data,
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::from(e)),
                };

                if input.is_empty() {
                    self.eof = true;
                    return Ok(false);
                }

                let (done, len) = match memchr::memchr(b'\0', input) {
                    Some(i) => (true, i + 1),
                    None => (false, input.len()),
                };

                let new_pos = *pos + len;
                if buf.resize(new_pos) {
                    buf[*pos..new_pos].copy_from_slice(&input[..len]);
                    *pos = new_pos;
                    (done, len)
                } else {
                    return Ok(false);
                }
            };
            self.reader.consume(len);
            if done {
                return Ok(true);
            }

        }
    }

    fn decode_prefix_diff(&mut self) -> Result<i16, Error> {
        let mut buf = [0; 1];
        self.reader.read_exact(&mut buf)?;

        if buf[0] != 0x80 {
            Ok((buf[0] as i8) as i16)
        } else {
            let mut buf = [0; 2];
            self.reader.read_exact(&mut buf)?;
            let high = buf[0] as i16;
            let low = buf[1] as i16;
            Ok(high << 8 | low)
        }
    }

    pub fn decode(&mut self) -> Result<&mut [u8], Error> {
        // If we found the end of the file, return an empty slice.
        if self.eof {
            return Ok(&mut []);
        }

        // save end pointer from previous iteration and reset write position
        let end = self.pos;
        self.pos = 0;

        let mut copy_pos = cmp::min(self.partial_item_start, self.last_path);
        let item_start = self.partial_item_start - copy_pos;

        // shift the last path, because we copy the data from copy_pos to 0
        self.last_path -= copy_pos;

        // Copy the last path and possible partial data from the current item
        // to the new buffer. We need access to the last path for copying the
        // common prefix.
        while copy_pos < end {
            self.buf[self.pos] = self.buf[copy_pos];
            self.pos += 1;
            copy_pos += 1;
        }

        // allow resizing the buffer, since we haven't decoded a full item yet
        self.buf.allow_resize = true;

        // If we haven't copied the shared data from the partial item yet, do it now
        if self.pos > 0 && self.buf[self.pos - 1] == b'\0' {
            self.copy_shared()?;
        }

        // Main decode loop
        loop {
            // Read data up to the next nul byte.
            if !self.read_to_nul()? {
                break;
            }

            // Parse the next prefix length difference
            let diff = self.decode_prefix_diff()? as isize;

            // Update the shared len
            self.shared_len = self.shared_len
                .checked_add(diff)
                .ok_or_else(|| {
                                Error::SharedOverflow {
                                    shared_len: self.shared_len,
                                    diff: diff,
                                }
                            })?;

            // Copy the shared prefix
            if !self.copy_shared()? {
                break;
            }
            self.buf.allow_resize = false;
        }

        // Find end of last item
        self.partial_item_start =
            memchr::memrchr(b'\n', &self.buf[..self.pos]).ok_or(Error::MissingNewline)? + 1;
        Ok(&mut self.buf[item_start..self.partial_item_start])
    }
}

pub struct Encoder<W: Write> {
    writer: W,
    last: Vec<u8>,
    shared_len: i16,
    footer_meta: Vec<u8>,
    footer_path: Vec<u8>,
    footer_written: bool,
}

impl<W: Write> Drop for Encoder<W> {
    fn drop(&mut self) {
        self.write_footer().expect("failed to write footer")
    }
}

impl<W: Write> Encoder<W> {
    pub fn new(writer: W, footer_meta: Vec<u8>, footer_path: Vec<u8>) -> Encoder<W> {
        assert!(!footer_meta.contains(&b'\x00'),
                "footer meta must not contain null bytes");
        assert!(!footer_path.contains(&b'\x00'),
                "footer path must not contain null bytes");
        Encoder {
            writer: writer,
            last: Vec::new(),
            shared_len: 0,
            footer_meta: footer_meta,
            footer_path: footer_path,
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

    pub fn write_meta(&mut self, meta: &[u8]) -> io::Result<()> {
        assert!(!meta.contains(&b'\x00'),
                "entry must not contain null bytes");

        self.writer.write_all(meta)?;
        Ok(())
    }

    pub fn write_path(&mut self, path: Vec<u8>) -> io::Result<()> {
        self.writer.write_all(&[b'\x00'])?;

        let mut shared: isize = 0;
        let max_shared = i16::max_value() as isize;
        for (a, b) in self.last.iter().zip(path.iter()) {
            if a != b || shared > max_shared {
                break;
            }
            shared += 1;
        }
        let shared = shared as i16;

        let diff = shared - self.shared_len;
        self.encode_diff(diff)?;

        self.last = path;
        self.shared_len = shared;

        let pos = shared as usize;
        self.writer.write_all(&self.last[pos..])?;
        self.writer.write_all(b"\n")?;

        Ok(())
    }

    fn write_footer(&mut self) -> io::Result<()> {
        if self.footer_written {
            return Ok(());
        }

        let diff = -self.shared_len;
        self.writer.write_all(&self.footer_meta)?;
        self.writer.write_all(b"\x00")?;
        self.encode_diff(diff)?;
        self.writer.write_all(&self.footer_path)?;
        self.writer.write_all(b"\n")?;
        self.footer_written = true;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.write_footer()?;

        Ok(())
    }
}
