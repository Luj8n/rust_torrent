use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::io::Read;
use std::path::Path;

use crate::bytes::sha1_hash;

#[derive(Clone, Debug)]
pub struct MetaInfo {
  pub announce: String,
  // pub announce_list: Option<Vec<String>>,
  pub creation_date: Option<u64>,
  pub comment: Option<String>,
  pub created_by: Option<String>,
  // pub encoding: Option<String>,
  pub piece_length: u64,
  pub pieces: Vec<[u8; 20]>,
  // pub private: Option<bool>,
  pub files: Vec<File>,
  pub info_hash: [u8; 20],
}

#[derive(Clone, Debug)]
pub struct File {
  pub length: u64,
  pub path: Vec<String>,
}

impl MetaInfo {
  pub fn from_file(path: &Path) -> Result<Self> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = vec![];
    file.read_to_end(&mut bytes)?;

    // dbg!(bytes.iter().map(|x| *x as char).collect::<String>());
    let bencode =
      BencodeRef::decode(&bytes, BDecodeOpt::default()).map_err(|e| anyhow!(e.to_string()))?;

    MetaInfo::from_bencode(bencode)
  }

  fn from_bencode(bencode: BencodeRef) -> Result<Self> {
    let root_dict = bencode.dict().ok_or(anyhow!("Bad metainfo file"))?;

    let announce = root_dict
      .lookup(b"announce")
      .map(|x| x.str().map(|y| y.to_owned()))
      .flatten()
      .ok_or(anyhow!("Announce missing"))?;

    // let announce_list = root_dict
    //   .lookup(b"announce-list")
    //   .map(|x| {
    //     x.list().map(|y| {
    //       y.into_iter()
    //         .map(|z| z.str().map(|y| y.to_owned()))
    //         .collect::<Option<Vec<String>>>()
    //     })
    //   })
    //   .flatten()
    //   .flatten();

    let creation_date = root_dict
      .lookup(b"creation date")
      .map(|x| x.int().map(|y| y.try_into().ok()))
      .flatten()
      .flatten();

    let comment = root_dict
      .lookup(b"comment")
      .map(|x| x.str().map(|y| y.to_owned()))
      .flatten();

    let created_by = root_dict
      .lookup(b"created by")
      .map(|x| x.str().map(|y| y.to_owned()))
      .flatten();

    // let encoding = root_dict
    //   .lookup(b"encoding")
    //   .map(|x| x.str().map(|y| y.to_owned()))
    //   .flatten();

    let info_dict = root_dict
      .lookup(b"info")
      .map(|x| x.dict())
      .flatten()
      .ok_or(anyhow!("Info missing"))?;

    let piece_length = info_dict
      .lookup(b"piece length")
      .map(|x| x.int().map(|y| y.try_into().ok()))
      .flatten()
      .flatten()
      .ok_or(anyhow!("Piece length missing or not a positive integer"))?;

    let pieces = info_dict
      .lookup(b"pieces")
      .map(|x| {
        x.bytes().map(|bytes| {
          // TODO: find a way to clean this up a bit
          let mut pieces: Vec<[u8; 20]> = vec![];

          for chunk in bytes.array_chunks() {
            pieces.push(chunk.to_owned());
          }

          pieces
        })
      })
      .flatten()
      .ok_or(anyhow!("Pieces missing"))?;

    // let private = info_dict
    //   .lookup(b"private")
    //   .map(|x| x.int().map(|y| y == 1))
    //   .flatten();

    let name = info_dict
      .lookup(b"name")
      .map(|x| x.str().map(|y| y.to_owned()))
      .flatten()
      .ok_or(anyhow!("Name missing"))?;

    let mut files: Vec<File> = vec![];

    let mut file_is_malformed = false;

    if let Some(length) = info_dict.lookup(b"length").map(|x| x.int()).flatten() {
      files.push(File {
        length: length
          .try_into()
          .map_err(|_| anyhow!("Length is negative"))?,
        path: vec![name.to_owned()],
      });
    } else {
      files = info_dict
        .lookup(b"files")
        .map(|x| {
          x.list().map(|y| {
            y.into_iter()
              .filter_map(|f| {
                let file_dict = f.dict().ok_or_else(|| file_is_malformed = true).ok()?;

                let length = file_dict
                  .lookup(b"length")
                  .map(|x| x.int())
                  .flatten()
                  .ok_or_else(|| file_is_malformed = true)
                  .ok()?;

                let mut path = file_dict
                  .lookup(b"path")
                  .map(|x| {
                    x.list().map(|y| {
                      y.into_iter()
                        .filter_map(|z| {
                          z.str()
                            .ok_or_else(|| file_is_malformed = true)
                            .ok()
                            .map(|o| o.to_owned())
                        })
                        .collect::<Vec<String>>()
                    })
                  })
                  .flatten()
                  .ok_or_else(|| file_is_malformed = true)
                  .ok()?;

                path.insert(0, name.clone());

                Some(File {
                  length: length
                    .try_into()
                    .map_err(|_| file_is_malformed = true)
                    .ok()?,
                  path,
                })
              })
              .collect::<Vec<File>>()
          })
        })
        .flatten()
        .ok_or(anyhow!("Files (and length) missing"))?;
    }

    if file_is_malformed {
      return Err(anyhow!("File dictionary is malformed"));
    }

    let info_bencoded = root_dict
      .lookup(b"info")
      .map(|x| x.buffer())
      .ok_or(anyhow!("Info missing or something"))?;

    let info_hash = sha1_hash(info_bencoded);

    Ok(MetaInfo {
      announce,
      creation_date,
      comment,
      created_by,
      piece_length,
      pieces,
      files,
      info_hash,
    })
  }
}
