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

pub struct TorrentManager {
  file_manager: FileManager,
  torrents: Vec<Torrent>,
}

impl TorrentManager {
  fn new() -> Self {
    let file_manager = FileManager::new();

    TorrentManager {
      file_manager,
      torrents: vec![],
    }
  }

  fn add_torrent(&mut self, bytes: &[u8]) -> Result<()> {
    let port = self.free_port()?;
    let torrent = Torrent::from_bytes(bytes, port, self.file_manager.sender.clone())?;
    self.torrents.push(torrent);

    Ok(())
  }

  fn add_torrent_from_file(&mut self, path: &Path) -> Result<()> {
    let bytes = from_file(path)?;

    self.add_torrent(&bytes)
  }

  fn free_port(&self) -> Result<u16> {
    todo!()
  }
}

pub struct FileManager {
  sender: Sender<Message>,
}

#[derive(Debug, Clone)]
pub enum Message {
  Write {
    bytes: Vec<u8>,
    file: Arc<fs::File>,
    offset: u64,
  },
}

impl FileManager {
  fn new() -> Self {
    let (tx, rx) = mpsc::channel::<Message>(32);

    tokio::spawn(async move {
      let mut rx = rx;

      while let Some(message) = rx.recv().await {
        use Message::*;

        println!("Got message = {message:?}");

        match message {
          Write {
            bytes,
            file,
            offset,
          } => {
            let result = file.write_all_at(&bytes, offset);
          }
        };
      }
    });

    FileManager { sender: tx }
  }
}
