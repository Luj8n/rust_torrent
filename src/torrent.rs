use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use crate::bytes::{encode_bytes, random_id};
use crate::manager::{FileManager, Message};
use crate::metainfo::{self, MetaInfo};

pub struct Torrent {
  pub metainfo: MetaInfo,
  pub port: u16,
  pub peer_id: [u8; 20],
  pub uploaded: u64,
  pub downloaded: u64,
  pub left: u64,

  file_manager_sender: Sender<Message>,
  files: Vec<FileInfo>,
}

#[derive(Clone, Debug)]
pub struct TrackerResponse {
  warning_message: Option<String>,
  interval: u64,
  min_interval: Option<u64>,
  tracker_id: Option<String>,
  complete: u64,
  incomplete: u64,
  peers: Vec<Peer>,
}

#[derive(Clone, Debug)]
pub struct Peer {
  ip: Ipv4Addr,
  port: u16,
}

pub enum Event {
  Started,
  Stopped,
  Completed,
}

impl ToString for Event {
  fn to_string(&self) -> String {
    match self {
      Event::Started => "started".into(),
      Event::Stopped => "stopped".into(),
      Event::Completed => "completed".into(),
    }
  }
}

#[derive(Debug)]
pub struct FileInfo {
  pub fs_file: Arc<fs::File>,
  pub length: u64,
  pub offset: u64, // offset from the start of the first file (in bytes)
}

impl Torrent {
  pub fn from_bytes(bytes: &[u8], port: u16, file_manager_sender: Sender<Message>) -> Result<Self> {
    let metainfo = MetaInfo::from_bytes(bytes)?;
    Ok(Torrent::from_metainfo(metainfo, port, file_manager_sender))
  }

  fn from_metainfo(metainfo: MetaInfo, port: u16, file_manager_sender: Sender<Message>) -> Self {
    let peer_id = random_id();
    let left = metainfo.files.iter().map(|x| x.length).sum();

    let mut files = vec![];
    let mut total_length = 0;

    for file in &metainfo.files {
      let mut dir = std::env::current_dir().unwrap();
      dir.push("/tmp");

      for name in &file.path {
        dir.push(name);
      }

      let fs_file = Arc::new(fs::File::open(dir).unwrap());
      let length = metainfo.piece_length;
      let offset = total_length;

      files.push(FileInfo {
        fs_file,
        length,
        offset,
      });

      total_length += length;
    }

    Torrent {
      metainfo,
      port,
      peer_id,
      uploaded: 0,
      downloaded: 0,
      left,
      file_manager_sender,
      files,
    }
  }

  pub async fn write_data(&self, bytes: Vec<u8>, offset: u64) {
    self
      .file_manager_sender
      .send(Message::Write {
        bytes,
        file: self.files[0].fs_file.clone(),
        offset,
      })
      .await
      .expect("Shouldn't fail sending message to file manager");
  }

  async fn write_chunk(&self, index: u64, begin: u64, piece: Vec<u8>) {
    todo!()
  }

  pub async fn request_tracker(
    &self,
    event: Option<Event>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    let protocol = self.metainfo.announce.split(':').next();

    match protocol {
      Some("https" | "http") => self.request_tracker_http(event, tracker_id).await,
      Some("udp") => self.request_tracker_udp(event, tracker_id).await,
      Some(_) => Err(anyhow!(
        "Tracker protocol not supported: {}",
        protocol.unwrap()
      )),
      None => Err(anyhow!(
        "Tracker url doesn't contain a protocol: {}",
        self.metainfo.announce
      )),
    }
  }

  async fn request_tracker_http(
    &self,
    event: Option<Event>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    // let url = format!("{}/?{}&{}{}", self.metainfo.announce);
    let mut builder = reqwest::Client::new()
      .get(&self.metainfo.announce)
      // .get("https://httpbin.org/anything")
      .query(&[("peer_id", encode_bytes(&self.peer_id))])
      .query(&[("port", self.port)])
      .query(&[
        ("uploaded", self.uploaded),
        ("downloaded", self.downloaded),
        ("left", self.left),
      ])
      .query(&[("compact", "1")]);

    if let Some(event) = event {
      builder = builder.query(&[("event", event.to_string())]);
    }

    if let Some(tracker_id) = tracker_id {
      builder = builder.query(&[("trackerid", tracker_id)]);
    }

    let url = builder.build().unwrap().url().to_string()
      + "&info_hash="
      + &encode_bytes(&self.metainfo.info_hash);

    let response = reqwest::Client::new()
      .get(url)
      .send()
      .await?
      .bytes()
      .await?;

    let bencode =
      BencodeRef::decode(&response, BDecodeOpt::default()).map_err(|e| anyhow!(e.to_string()))?;

    let root_dict = bencode
      .dict()
      .ok_or_else(|| anyhow!("Couldn't make dict"))?;

    if let Some(failure_reason) = root_dict
      .lookup(b"failure reason")
      .and_then(|x| x.str().map(|y| y.to_owned()))
    {
      return Err(anyhow!(failure_reason));
    }

    let warning_message = root_dict
      .lookup(b"warning message")
      .and_then(|x| x.str().map(|y| y.to_owned()));

    let interval = root_dict
      .lookup(b"interval")
      .and_then(|x| x.int().map(|y| y.try_into().ok()))
      .flatten()
      .ok_or_else(|| anyhow!("Interval missing"))?;

    let min_interval = root_dict
      .lookup(b"min interval")
      .and_then(|x| x.int().map(|y| y.try_into().ok()))
      .flatten();

    let tracker_id = root_dict
      .lookup(b"tracker id")
      .and_then(|x| x.str().map(|y| y.to_owned()));
    // .ok_or_else(|| anyhow!("Tracker id missing"))?;

    let complete = root_dict
      .lookup(b"complete")
      .and_then(|x| x.int().map(|y| y.try_into().ok()))
      .flatten()
      .ok_or_else(|| anyhow!("Complete missing"))?;

    let incomplete = root_dict
      .lookup(b"incomplete")
      .and_then(|x| x.int().map(|y| y.try_into().ok()))
      .flatten()
      .ok_or_else(|| anyhow!("Complete missing"))?;

    let peers_bytes = root_dict
      .lookup(b"peers")
      .and_then(|x| x.bytes())
      .ok_or_else(|| anyhow!("Incomplete missing"))?;

    let peers = peers_bytes
      .array_chunks::<6>()
      .map(|[a, b, c, d, e, f]| Peer {
        ip: Ipv4Addr::new(*a, *b, *c, *d),
        port: ((*e as u16) << 8) + (*f as u16),
      })
      .collect();

    Ok(TrackerResponse {
      warning_message,
      interval,
      min_interval,
      tracker_id,
      complete,
      incomplete,
      peers,
    })
  }

  pub async fn request_tracker_udp(
    &self,
    event: Option<Event>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    todo!()
  }
}
