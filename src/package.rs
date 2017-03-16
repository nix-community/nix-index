use std::io::{self, Write};
use std::borrow::{Cow};
use std::str;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathOrigin {
    pub attr: String,
    pub output: String,
    pub toplevel: bool,
}

impl PathOrigin {
    pub fn encode<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        write!(writer, "{}\x02{}{}", self.attr, self.output, if self.toplevel { "" } else { "\x02" })?;
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> Option<PathOrigin> {
        let mut iter = buf.splitn(2, |c| *c == b'\x02');
        iter.next().and_then(|v| String::from_utf8(v.to_vec()).ok()).and_then(|attr| {
            iter.next().and_then(|v| String::from_utf8(v.to_vec()).ok()).and_then(|mut output| {
                let mut toplevel = true;
                if let Some(l) = output.pop() {
                    if l == '\x02' { toplevel = false }
                    else { output.push(l) }
                }
                Some(PathOrigin { attr: attr, output: output, toplevel: toplevel })
            })
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    store_dir: String,
    hash: String,
    name: String,
    origin: PathOrigin,
}

impl StorePath {
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

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut result = Vec::with_capacity(self.as_str().len());
        result.extend(self.as_str().bytes());
        result.push(b'\n');
        self.origin().encode(&mut result)?;
        Ok(result)
    }

    pub fn decode(buf: &[u8]) -> Option<StorePath> {
        let mut parts = buf.splitn(2, |c| *c == b'\n');
        parts.next().and_then(|v| str::from_utf8(v).ok()).and_then(|path| {
            parts.next().and_then(PathOrigin::decode).and_then(|origin| {
                StorePath::parse(origin, path)
            })
        })
    }

    pub fn name(&self) -> Cow<str> { Cow::Borrowed(&self.name) }
    pub fn hash(&self) -> Cow<str> { Cow::Borrowed(&self.hash) }
    pub fn store_dir(&self) -> Cow<str> { Cow::Borrowed(&self.store_dir) }
    pub fn as_str(&self) -> Cow<str> {
        Cow::Owned(format!("{}/{}-{}", self.store_dir, self.hash, self.name))
    }
    pub fn origin(&self) -> Cow<PathOrigin> { Cow::Borrowed(&self.origin) }
}
