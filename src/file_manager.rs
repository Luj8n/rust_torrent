use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::prelude::FileExt;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::oneshot;

use crate::bytes::{encode_bytes, from_file, random_id};
use crate::metainfo::{File, MetaInfo};
use crate::torrent::Torrent;

pub struct FileManager {
  sender: Sender<FileManagerMessage>,
}

#[derive(Debug, Clone)]
pub enum FileManagerMessage {
  Write {
    bytes: Vec<u8>,
    file: Arc<fs::File>,
    offset: u64,
  },
  Read {
    file: Arc<fs::File>,
    offset: u64,
    receiver: usize, // some type
  },
}

impl FileManager {
  pub fn new() -> Self {
    let (tx, rx) = mpsc::channel::<FileManagerMessage>(32);

    tokio::spawn(async move {
      let mut rx = rx;

      while let Some(message) = rx.recv().await {
        use FileManagerMessage::*;

        println!("Got message = {message:?}");

        match message {
          Write {
            bytes,
            file,
            offset,
          } => {
            // TODO: probably should buffer this
            let result = file.write_all_at(&bytes, offset);

            // file.read_at(buf, offset)
          }
          Read {
            file,
            offset,
            receiver,
          } => {
            // flush buffer if we have it
          }
          _ => {}
        };
      }
    });

    FileManager { sender: tx }
  }

  pub fn get_sender(&self) -> Sender<FileManagerMessage> {
    self.sender.clone()
  }
}
