use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use futures::future::join_all;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Weak};
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{oneshot, Mutex};
use tokio::time::{self, Duration};

use crate::bytes::{encode_bytes, random_id, sha1_hash};
use crate::file_manager::FileManagerMessage;
use crate::metainfo::MetaInfo;
use crate::peer::Peer;

pub struct Torrent {
  pub metainfo: MetaInfo,
  port: u16,
  pub peer_id: [u8; 20],
  files: Vec<FileInfo>, // TODO: later add a feature to choose which files to download
  file_manager_sender: Sender<FileManagerMessage>,

  bytes_uploaded: Mutex<u64>,
  bytes_downloaded: Mutex<u64>,
  peers: Mutex<Vec<Arc<Peer>>>, // TODO: probably hashmap better
  pub downloaded_pieces: Mutex<Vec<bool>>, // TODO: maybe store other piece data, like how many peers have them (if it is too slow to do it every second)

  torrent: Weak<Torrent>,
}

#[derive(Clone, Debug)]
pub struct TrackerResponse {
  warning_message: Option<String>,
  interval: u64,
  min_interval: Option<u64>,
  tracker_id: Option<String>,
  complete: u64,
  incomplete: u64,
  peer_info: Vec<SocketAddr>,
}

pub enum TrackerEvent {
  Started,
  Stopped,
  Completed,
}

impl ToString for TrackerEvent {
  fn to_string(&self) -> String {
    match self {
      TrackerEvent::Started => "started".into(),
      TrackerEvent::Stopped => "stopped".into(),
      TrackerEvent::Completed => "completed".into(),
    }
  }
}

#[derive(Debug)]
pub struct FileInfo {
  pub file: Arc<fs::File>,
  pub length: u64,
  pub offset: u64, // offset from the start of the first file (in bytes)
}

#[derive(Debug)]
pub enum TorrentMessage {}

impl Torrent {
  pub fn from_bytes(
    bytes: &[u8],
    port: u16,
    file_manager_sender: Sender<FileManagerMessage>,
  ) -> Result<Arc<Self>> {
    let metainfo = MetaInfo::from_bytes(bytes)?;
    Torrent::from_metainfo(metainfo, port, file_manager_sender)
  }

  fn from_metainfo(
    metainfo: MetaInfo,
    port: u16,
    file_manager_sender: Sender<FileManagerMessage>,
  ) -> Result<Arc<Self>> {
    let peer_id = random_id();

    let mut files = vec![];
    let mut total_length = 0;

    for file in &metainfo.files {
      let mut dir = std::env::current_dir().unwrap();
      dir.push("tmp");

      for name in &file.path[0..file.path.len() - 1] {
        dir.push(name);
      }
      fs::create_dir_all(&dir)?;

      dir.push(
        file
          .path
          .last()
          .ok_or_else(|| anyhow!("Path is too short"))?,
      );

      // TODO: open existing file
      let open_file = fs::File::options()
        .write(true)
        .read(true)
        .create(true)
        .open(&dir)
        .unwrap();

      let length = file.length;
      let offset = total_length;

      files.push(FileInfo {
        file: Arc::new(open_file),
        length,
        offset,
      });

      total_length += length;
    }

    let pieces_len = metainfo.pieces.len();

    Ok(Arc::new_cyclic(|me| Torrent {
      port,
      files,
      metainfo,
      peer_id,
      file_manager_sender,

      bytes_uploaded: Mutex::new(0),
      bytes_downloaded: Mutex::new(0),
      peers: Mutex::new(vec![]),
      downloaded_pieces: Mutex::new(vec![false; pieces_len]),

      torrent: me.clone(),
    }))
  }

  pub fn start_downloading(&self) -> Sender<TorrentMessage> {
    let torrent = self.torrent.upgrade().unwrap();

    let (tx, mut rx) = mpsc::channel::<TorrentMessage>(32);

    tokio::spawn(async move {
      // TODO: move hash checking somewhere else
      // check hash, set downloaded pieces
      let good_pieces = torrent.check_whole_hash().await;
      *torrent.downloaded_pieces.lock().await = good_pieces;

      let listener = TcpListener::bind(format!("127.0.0.1:{}", torrent.port))
        .await
        .expect("Couldn't bind"); // TODO: remove .expect;
      let mut interval = time::interval(Duration::from_millis(1000)); // TODO: experiment with other interval durations

      let first_tracker_response = torrent
        .request_tracker(Some(TrackerEvent::Started), None)
        .await
        .expect("Couldn't request tracker"); // TODO: remove .expect

      let peers: Vec<Arc<Peer>> = first_tracker_response
        .peer_info
        .iter()
        .map(|a| Peer::new(*a, Arc::downgrade(&torrent)))
        .collect();

      // for peer in peers {
      //   peer.
      // }

      *torrent.peers.lock().await = peers;

      loop {
        select! {
          // update stats every second*
          _ = interval.tick() => {
            // TODO: calculate rolling peer download/upload speed averages
            // TODO: choke and unchoke peers
          }

          // listen for new peers joining
          Ok((stream, socket)) = listener.accept() => {
              // TODO: add peer
          }

          // an mpsc channel to listen for peer updates and update their stats
          Some(msg) = rx.recv() => {
            match msg {

            }
          }
          // TODO: add a timer for the tracker
        };
      }
    });

    tx
  }

  pub async fn pause(&mut self) -> Result<()> {
    todo!()
  }

  pub async fn get_statistics(&self) -> Result<()> {
    todo!()
  }

  fn get_file_sections(&self, offset: u64, length: u64) -> Vec<(&FileInfo, u64, u64)> {
    // (file, offset from file start, length)

    let byte_start_offset = offset;
    let byte_end_offset = byte_start_offset + length;

    let mut files = vec![];

    for file in &self.files {
      let file_start_offset = file.offset;
      let file_end_offset = file_start_offset + file.length;

      if byte_start_offset >= file_end_offset || byte_end_offset <= file_start_offset {
        continue;
      }

      let intersection_start_offset = file_start_offset.max(byte_start_offset);
      let intersection_end_offset = file_end_offset.min(byte_end_offset);

      files.push((
        file,
        intersection_start_offset - file_start_offset,
        intersection_end_offset - intersection_start_offset,
      ));
    }

    files
  }

  async fn check_piece_hash(&self, piece_index: u32) -> bool {
    let bytes = join_all(
      self
        .get_file_sections(
          piece_index as u64 * self.metainfo.piece_length,
          self.metainfo.piece_length,
        )
        .iter()
        .map(|f| self.read_data(f.0, f.1, f.2)),
    )
    .await
    .concat();

    let hashed = sha1_hash(&bytes);

    hashed == self.metainfo.pieces[piece_index as usize]
  }

  async fn check_whole_hash(&self) -> Vec<bool> {
    let mut v = vec![];

    for i in 0..self.metainfo.pieces.len() {
      v.push(self.check_piece_hash(i as u32).await);
    }

    v
  }

  pub async fn read_chunk(&self, piece_index: u32, offset: u64, length: u64) -> Vec<u8> {
    // TODO: could implement some caching - save the whole piece in memory
    join_all(
      self
        .get_file_sections(
          piece_index as u64 * self.metainfo.piece_length + offset,
          length,
        )
        .iter()
        .map(|f| self.read_data(f.0, f.1, f.2)),
    )
    .await
    .concat()
  }

  async fn write_chunk(&self, piece_index: u32, offset: u64, bytes: Vec<u8>) {
    let sections = self.get_file_sections(
      piece_index as u64 * self.metainfo.piece_length + offset,
      bytes.len() as u64,
    );

    let mut cur_byte_offset = 0;

    for (file_info, chunk_offset, chunk_length) in sections {
      self
        .write_data(
          file_info,
          chunk_offset,
          bytes[cur_byte_offset as usize..(cur_byte_offset + chunk_length) as usize].to_vec(),
        )
        .await;

      cur_byte_offset += chunk_length;
    }
  }

  async fn write_data(&self, file_info: &FileInfo, offset: u64, bytes: Vec<u8>) {
    self
      .file_manager_sender
      .send(FileManagerMessage::Write {
        bytes,
        file: file_info.file.clone(),
        offset,
      })
      .await
      .expect("Shouldn't fail sending message to file manager");
  }

  async fn read_data(&self, file_info: &FileInfo, offset: u64, length: u64) -> Vec<u8> {
    let (sender, receiver) = oneshot::channel();

    self
      .file_manager_sender
      .send(FileManagerMessage::Read {
        file: file_info.file.clone(),
        offset,
        length,
        sender,
      })
      .await
      .expect("Shouldn't fail sending message to file manager");

    receiver.await.unwrap()
  }

  async fn bytes_left(&self) -> u64 {
    self
      .downloaded_pieces
      .lock()
      .await
      .iter()
      .filter(|b| **b == false)
      .count() as u64
      * self.metainfo.piece_length
  }

  async fn request_tracker(
    &self,
    event: Option<TrackerEvent>,
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
    event: Option<TrackerEvent>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    // let url = format!("{}/?{}&{}{}", self.metainfo.announce);
    let mut builder = reqwest::Client::new()
      .get(&self.metainfo.announce)
      // .get("https://httpbin.org/anything")
      .query(&[("peer_id", encode_bytes(&self.peer_id))])
      .query(&[("port", self.port)])
      .query(&[
        ("uploaded", *self.bytes_uploaded.lock().await),
        ("downloaded", *self.bytes_downloaded.lock().await),
        ("left", self.bytes_left().await),
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
      .chunks_exact(6)
      .map(|x| {
        // TODO: this is just meh
        SocketAddr::new(
          IpAddr::V4(Ipv4Addr::new(x[0], x[1], x[2], x[3])),
          ((x[4] as u16) << 8) + (x[5] as u16),
        )
      })
      .collect();

    Ok(TrackerResponse {
      warning_message,
      interval,
      min_interval,
      tracker_id,
      complete,
      incomplete,
      peer_info: peers,
    })
  }

  async fn request_tracker_udp(
    &self,
    event: Option<TrackerEvent>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    todo!()
  }
}
