use std::collections::{HashMap};
use serde::bytes::{ByteBuf};
use grep::{Grep};

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub enum Files {
    Leaf(File),
    Directory {
        entries: HashMap<ByteBuf, Files>,
    },
    Empty,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub enum File {
    Regular { size: u64, executable: bool },
    Symlink { target: ByteBuf },
}

pub struct FileWithPath {
    pub path: Vec<u8>,
    pub file: File,
}

impl Files {
    pub fn is_leaf(&self) -> bool {
        use self::Files::*;
        match self {
            &Leaf(..) | &Empty => true,
            &Directory {..} => false,
        }
    }

    pub fn empty() -> Files {
        Files::Empty
    }

    pub fn is_empty(&self) -> bool {
        match self {
            &Files::Empty => true,
            _ => false,
        }
    }

    pub fn to_list(&self) -> Vec<FileWithPath> {
        use self::Files::*;

        let mut result = Vec::new();

        let mut stack = Vec::with_capacity(16);
        stack.push((Vec::new(), self));

        while let Some((path, current)) = stack.pop() {
            match current {

                &Leaf(ref file) => {
                    result.push(FileWithPath { file: file.clone(), path: path });
                },
                &Directory { ref entries } => {
                    for (name, entry) in entries {
                        let mut path = path.clone();
                        path.push(b'/');
                        path.extend_from_slice(name);
                        stack.push((path, &entry));
                    }
                },
                &Empty => {}
            }
        }
        result
    }

    pub fn grep(&self, grep: &Grep) -> Vec<FileWithPath> {
        let files = self.to_list();
        files.into_iter().filter(|file| {
            grep.regex().find(&file.path).is_some()
        }).collect()
    }
}
