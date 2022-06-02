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
use crate::file_manager::{FileManager, FileManagerMessage};
use crate::metainfo::{File, MetaInfo};
use crate::torrent::Torrent;

pub struct TorrentManager {
  file_manager: FileManager,
  torrents: Vec<Torrent>,
}

impl TorrentManager {
  pub fn new() -> Self {
    let file_manager = FileManager::new();

    TorrentManager {
      file_manager,
      torrents: vec![],
    }
  }

  fn add_torrent(&mut self, bytes: &[u8]) -> Result<()> {
    let port = self.free_port()?;
    let torrent = Torrent::from_bytes(bytes, port, self.file_manager.get_sender())?;
    self.torrents.push(torrent);

    Ok(())
  }

  pub fn add_torrent_from_file(&mut self, path: &Path) -> Result<()> {
    let bytes = from_file(path)?;

    self.add_torrent(&bytes)
  }

  pub async fn download_torrent(&mut self, index: usize) -> Result<()> {
    if index >= self.torrents.len() {
      return Err(anyhow!("Index out of torrent vec bounds"));
    };

    self.torrents[index].start_downloading().await;

    Ok(())
  }

  fn free_port(&self) -> Result<u16> {
    // todo!()
    Ok(6969)
  }
}
