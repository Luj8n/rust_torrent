use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use tokio::sync::mpsc;

use crate::bytes::{encode_bytes, from_file, random_id};
use crate::metainfo::MetaInfo;
use crate::torrent::Torrent;

pub struct TorrentManager {
  torrents: Vec<Torrent>,
}

impl TorrentManager {
  fn new() -> Self {
    TorrentManager { torrents: vec![] }
  }

  fn add_torrent(&mut self, bytes: &[u8]) -> Result<()> {
    let port = self.free_port()?;
    let torrent = Torrent::from_bytes(bytes, port)?;
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

pub struct FileManager {}

pub struct Message {}

impl FileManager {
  fn new() -> Self {
    let (tx, mut rx) = mpsc::channel::<Message>(32);

    todo!()
  }
}
