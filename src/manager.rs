use anyhow::Result;
use std::path::Path;

use crate::bytes::from_file;
use crate::file_manager::FileManager;
use crate::torrent::Torrent;

pub struct TorrentManager {
  pub torrents: Vec<Torrent>, // TODO: maybe a vector is not the best structure
  file_manager: FileManager,
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

  fn free_port(&self) -> Result<u16> {
    // TODO
    Ok(6969)
  }
}
