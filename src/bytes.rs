use anyhow::{anyhow, Result};
use rand::prelude::*;
use sha1::{Digest, Sha1};
use std::io::Read;
use std::path::Path;

pub fn sha1_hash(bytes: &[u8]) -> [u8; 20] {
  let mut hasher = Sha1::new();
  hasher.update(bytes);
  let hashed = hasher.finalize();
  hashed.try_into().unwrap()
}

pub fn random_id() -> [u8; 20] {
  let mut bytes = [0; 20];
  for b in &mut bytes {
    let n = thread_rng().gen_range(0..3);
    if n == 0 {
      *b = thread_rng().gen_range(65..=90);
    } else if n == 1 {
      *b = thread_rng().gen_range(97..=122);
    } else if n == 2 {
      *b = thread_rng().gen_range(48..=57);
    }
  }
  bytes
}

pub fn random_bytes() -> [u8; 20] {
  let mut bytes = [0; 20];
  thread_rng().fill(&mut bytes);
  bytes
}

pub fn encode_bytes(bytes: &[u8]) -> String {
  urlencoding::encode_binary(bytes).to_string()

  // let mut out: String = "".to_string();
  // for b in bytes {
  //   match *b as char {
  //     '0'..='9' | 'a'..='z' | 'A'..='Z' | '.' | '-' | '_' | '~' => out.push(*b as char),
  //     _ => out += &format!("%{:02X}", b),
  //   };
  // }

  // out
}

pub fn bytes_to_hexadecimal(bytes: &[u8]) -> String {
  // returns in uppercase
  bytes
    .iter()
    .flat_map(|b| {
      [
        nibble_to_hexadeximal(b >> 4),
        nibble_to_hexadeximal(b & 0b1111),
      ]
    })
    .collect()
}

fn nibble_to_hexadeximal(nibble: u8) -> char {
  match nibble {
    0..=9 => (nibble + 48) as char,
    10..=15 => (nibble - 10 + 65) as char,
    _ => panic!("Not a nibble"),
  }
}

pub fn from_file(path: &Path) -> Result<Vec<u8>> {
  let mut file = std::fs::File::open(path)?;
  let mut bytes = vec![];
  file.read_to_end(&mut bytes)?;

  Ok(bytes)
}
