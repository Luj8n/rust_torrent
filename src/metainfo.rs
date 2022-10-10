use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};

use crate::bytes::sha1_hash;

#[derive(Clone, Debug)]
pub struct MetaInfo {
  pub announce: String,
  pub announce_list: Option<Vec<String>>,
  pub creation_date: Option<u32>,
  pub comment: Option<String>,
  pub created_by: Option<String>,
  // pub encoding: Option<String>,
  pub piece_length: u32,
  pub pieces: Vec<[u8; 20]>,
  // pub private: Option<bool>,
  pub files: Vec<File>,
  pub info_hash: [u8; 20],
}

#[derive(Clone, Debug)]
pub struct File {
  pub length: u32,
  pub path: Vec<String>,
}

impl MetaInfo {
  pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
    let bencode =
      BencodeRef::decode(bytes, BDecodeOpt::default()).map_err(|e| anyhow!(e.to_string()))?;

    MetaInfo::from_bencode(bencode)
  }

  fn from_bencode(bencode: BencodeRef) -> Result<Self> {
    let root_dict = bencode.dict().ok_or_else(|| anyhow!("Bad metainfo file"))?;

    let announce = root_dict
      .lookup(b"announce")
      .and_then(|x| x.str().map(|y| y.to_owned()))
      .ok_or_else(|| anyhow!("Announce missing"))?;

    let announce_list = root_dict
      .lookup(b"announce-list")
      .map(|x| {
        x.list().map(|y| {
          y.into_iter()
            .map(|z| z.str().map(|y| y.to_owned()))
            .collect::<Option<Vec<String>>>()
        })
      })
      .flatten()
      .flatten();

    let creation_date = root_dict
      .lookup(b"creation date")
      .and_then(|x| x.int().map(|y| y.try_into().ok()))
      .flatten();

    let comment = root_dict
      .lookup(b"comment")
      .and_then(|x| x.str().map(|y| y.to_owned()));

    let created_by = root_dict
      .lookup(b"created by")
      .and_then(|x| x.str().map(|y| y.to_owned()));

    // let encoding = root_dict
    //   .lookup(b"encoding")
    //   .and_then(|x| x.str().map(|y| y.to_owned()));

    let info_dict = root_dict
      .lookup(b"info")
      .and_then(|x| x.dict())
      .ok_or_else(|| anyhow!("Info missing"))?;

    let piece_length = info_dict
      .lookup(b"piece length")
      .and_then(|x| x.int().map(|y| y.try_into().ok()))
      .flatten()
      .ok_or_else(|| anyhow!("Piece length missing or not a positive integer"))?;

    let pieces = info_dict
      .lookup(b"pieces")
      .and_then(|x| {
        x.bytes().map(|bytes| {
          // TODO: find a way to clean this up a bit
          let mut pieces: Vec<[u8; 20]> = vec![];

          for chunk in bytes.chunks_exact(20) {
            pieces.push(chunk.try_into().unwrap());
          }

          pieces
        })
      })
      .ok_or_else(|| anyhow!("Pieces missing"))?;

    // let private = info_dict
    //   .lookup(b"private")
    //   .map(|x| x.int().map(|y| y == 1))
    //   .flatten();

    let name = info_dict
      .lookup(b"name")
      .and_then(|x| x.str().map(|y| y.to_owned()))
      .ok_or_else(|| anyhow!("Name missing"))?;

    let mut files: Vec<File> = vec![];

    let mut file_is_malformed = false;

    if let Some(length) = info_dict.lookup(b"length").and_then(|x| x.int()) {
      files.push(File {
        length: length
          .try_into()
          .map_err(|_| anyhow!("Length is negative"))?,
        path: vec![name],
      });
    } else {
      files = info_dict
        .lookup(b"files")
        .and_then(|x| {
          x.list().map(|y| {
            y.into_iter()
              .filter_map(|f| {
                let file_dict = f.dict().ok_or_else(|| file_is_malformed = true).ok()?;

                let length = file_dict
                  .lookup(b"length")
                  .and_then(|x| x.int())
                  .ok_or_else(|| file_is_malformed = true)
                  .ok()?;

                let mut path = file_dict
                  .lookup(b"path")
                  .and_then(|x| {
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
        .ok_or_else(|| anyhow!("Files (and length) missing"))?;
    }

    if file_is_malformed {
      return Err(anyhow!("File dictionary is malformed"));
    }

    let info_bencoded = root_dict
      .lookup(b"info")
      .map(|x| x.buffer())
      .ok_or_else(|| anyhow!("Info missing or something"))?;

    let info_hash = sha1_hash(info_bencoded);

    Ok(MetaInfo {
      announce,
      announce_list,
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
