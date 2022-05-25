use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::fs::File as FsFile;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::oneshot;

use crate::bytes::{encode_bytes, from_file, random_id};
use crate::metainfo::{File, MetaInfo};
use crate::torrent::Torrent;

const DOWNLOAD_FOLDER: &str = "./tmp/";

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
    // file: &FsFile, // TODO
    file: File,
    offset: u64,
  },
}

impl FileManager {
  fn new() -> Self {
    let (tx, rx) = mpsc::channel::<Message>(32);

    tokio::spawn(async move {
      let mut rx = rx;
      let mut open_files: Vec<(FsFile, String)> = vec![];

      while let Some(message) = rx.recv().await {
        use Message::*;

        println!("Got message = {message:?}");

        match message {
          Write {
            bytes,
            file,
            offset,
          } => {
            // let mut dir = std::env::current_dir().unwrap();
            // dir.push(DOWNLOAD_FOLDER);

            // for name in &file.path {
            //   dir.push(name);
            // }

            // let mut file = FsFile::open(dir).unwrap();

            // TODO move this to torrent impl

            let file_path = file
              .path
              .iter()
              .fold(DOWNLOAD_FOLDER.to_string(), |a, c| a + "/" + c);

            let file = {
              if let Some((file, _)) = open_files.iter().find(|(_, p)| *p == file_path) {
                file
              } else {
                let file = FsFile::open(&file_path);

                if file.is_err() {
                  eprintln!("Couldn't create file, file path = {file_path}");
                  continue;
                }

                open_files.push((file.unwrap(), file_path));

                &open_files.last().unwrap().0
              }
            };
          }
        };
      }
    });

    FileManager { sender: tx }
  }

  async fn write_bytes(&self, bytes: Vec<u8>, file: File, offset: u64) -> Result<()> {
    self
      .sender
      .send(Message::Write {
        bytes,
        file,
        offset,
      })
      .await?;

    Ok(())
  }
}
