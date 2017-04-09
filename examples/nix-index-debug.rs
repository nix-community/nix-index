extern crate nix_index;

use nix_index::database::Reader;

fn main() {
    let f = std::env::args().nth(1).expect("file name given as 1st arg");
    let mut db = Reader::open(f).unwrap();
    db.dump().unwrap();
}
