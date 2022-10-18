use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use futures::future::join_all;
use rand::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Weak};
use tokio::net::{TcpListener, UdpSocket};
use tokio::select;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{oneshot, Mutex};
use tokio::time::{self, Duration};

use crate::bytes::{encode_bytes, random_id, sha1_hash};
use crate::constants;
use crate::file_manager::FileManagerMessage;
use crate::metainfo::MetaInfo;
use crate::peer::{Block, BlockInfo, ClientMessage, Peer};

pub struct Torrent {
  pub metainfo: MetaInfo,
  port: u16,
  pub peer_id: [u8; 20],
  files: Vec<FileInfo>, // TODO: later add a feature to choose which files to download
  file_manager_sender: Sender<FileManagerMessage>,

  // TODO: update these stats
  bytes_uploaded: Mutex<u32>,
  bytes_downloaded: Mutex<u32>,
  peers: Mutex<Vec<Arc<Peer>>>, // TODO: probably hashmap better
  pub downloaded_pieces: Mutex<Vec<bool>>, // TODO: maybe store other piece data, like how many peers have them (if it is too slow to do it every second)

  pub blocks_to_download: Mutex<Vec<BlockInfo>>,
  torrent: Weak<Torrent>,

  downloading_piece_index: Mutex<Option<u32>>,
  downloading_piece: Mutex<Vec<Option<Block>>>,

  optimistic_unchoke_peer: Mutex<Option<SocketAddr>>,
}

#[derive(Clone, Debug)]
pub struct TrackerResponse {
  warning_message: Option<String>,
  interval: u32,
  min_interval: Option<u32>,
  tracker_id: Option<String>,
  complete: u32,
  incomplete: u32,
  peer_info: Vec<SocketAddr>,
}

#[derive(Debug, Clone, Copy)]
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
  pub length: u32,
  pub offset: u32, // offset from the start of the first file (in bytes)
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

      blocks_to_download: Mutex::new(vec![]),
      torrent: me.clone(),

      downloading_piece_index: Mutex::new(None),
      downloading_piece: Mutex::new(vec![]),

      optimistic_unchoke_peer: Mutex::new(None),
    }))
  }

  pub fn start_downloading(&self) -> Sender<TorrentMessage> {
    println!("Starting the download");

    let torrent = self.torrent.upgrade().unwrap();

    let (tx, mut rx) = mpsc::channel::<TorrentMessage>(32);

    let tx_clone = tx.clone();

    tokio::spawn(async move {
      // TODO: move hash checking somewhere else
      // check hash, set downloaded pieces
      println!("Checking torrent hash");
      // TODO: if the file is too big, shorten it
      let good_pieces = torrent.check_whole_hash().await;
      *torrent.downloaded_pieces.lock().await = good_pieces;

      println!("Starting to listen for new peers");
      let listener = TcpListener::bind(format!("127.0.0.1:{}", torrent.port))
        .await
        .expect("Couldn't bind"); // TODO: remove .expect;

      println!("Contacting the tracker");
      let first_tracker_response = torrent
        .request_tracker(Some(TrackerEvent::Started), None)
        .await
        .expect("Couldn't request tracker"); // TODO: remove .expect

      dbg!(&first_tracker_response);
      println!("Our port = {}", torrent.port);

      println!("Adding peers");
      let peers: Vec<Arc<Peer>> = first_tracker_response
        .peer_info
        .iter()
        .filter(|a| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), torrent.port) != **a)
        .map(|a| Peer::new(*a, Arc::downgrade(&torrent), tx.clone()))
        .collect();

      for peer in &peers {
        peer.start(None).unwrap();
      }

      *torrent.peers.lock().await = peers;

      torrent.choose_first_piece().await;

      torrent.alert_peers_updated_job_queue().await;

      let mut choke_interval = time::interval(Duration::from_secs(10));
      let mut optimistic_unchoke_interval = time::interval(Duration::from_secs(30));
      let mut tracker_interval =
        time::interval(Duration::from_secs(first_tracker_response.interval as u64));

      loop {
        select! {
          _ = choke_interval.tick() => {
            {
              let downloaded_pieces = torrent.downloaded_pieces.lock().await;
              let progress = 100.0 * downloaded_pieces.iter().filter(|x| **x).count() as f32 / downloaded_pieces.len() as f32;

              println!();
              println!("--- Progress: {:.1}/100", progress);
            }

            torrent.regular_unchoke().await;
          }
          _ = optimistic_unchoke_interval.tick() => {
            // optimistically unchoke a random peer
            // TODO: add bias (3x) to new peers

            let mut optimistic_unchoke_peer = torrent.optimistic_unchoke_peer.lock().await;

            let peers = torrent.peers.lock().await;

            // choke the old optimistically unchoked peer
            if let Some(old_peer) = peers.iter().find(|x| Some(x.address) == *optimistic_unchoke_peer) {
              old_peer.send_message(ClientMessage::Choke).await;
            }

            //                                  am_choking, peer_interested
            let mut peer_map: HashMap<SocketAddr, (bool, bool)> = HashMap::new();
            for peer in &*peers {
              let peer_info = peer.peer_info.lock().await;
              peer_map.insert(peer.address, (peer_info.am_choking, peer_info.peer_interested));
            }

            // for some reason it has to be a seperate variable
            let new_unchoke = peers
              .iter()
              .filter(|x| Some(x.address) != *optimistic_unchoke_peer && peer_map[&x.address].0) //  TODO: && peer_map[&x.address].1 ???
              .choose(&mut thread_rng());

            if let Some(new_unchoke) = &new_unchoke {
              // unchoke the new optimistically unchoked peer
              new_unchoke.send_message(ClientMessage::Unchoke).await;

              *optimistic_unchoke_peer = Some(new_unchoke.address);

              println!("Optimistically unchoked {}", new_unchoke.address);
            } else {
              *optimistic_unchoke_peer = None;
            }
          }

          // listen for new peers joining
          Ok((stream, socket)) = listener.accept() => {
            println!("New peer joined");

            let new_peer = Peer::new(socket, Arc::downgrade(&torrent), tx.clone());
            new_peer.start(Some(stream)).unwrap();
            torrent.peers.lock().await.push(new_peer);
          }

          // an mpsc channel to listen for peer updates and update their stats
          Some(msg) = rx.recv() => {
            use TorrentMessage::*;
            match msg {

            }
          }
          _ = tracker_interval.tick() => {
            // TODO: add a timer for the tracker

          }
        };
      }
    });

    tx_clone
  }

  pub async fn interested_peer(&self) {
    // Peers which have a better upload rate (as compared to the downloaders) but aren't interested get unchoked.
    // If they become interested, the regular unchoke algorithm is run

    // run regular unchoke algorithm
    // self.regular_unchoke().await;
    // TODO: maybe reset regular unchoke timer
    // choke_interval.reset();
  }

  pub async fn block_downloaded(&self, block: Block) {
    let block_info = block.info;

    self.downloading_piece.lock().await[(block_info.begin / constants::BLOCK_SIZE) as usize] =
      Some(block);

    // check if piece is finished
    self.try_choose_new_piece().await;
  }

  pub async fn disconnect_peer(&self, address: SocketAddr) {
    self.peers.lock().await.retain(|x| x.address != address);

    // run regular unchoke algorithm
    // self.regular_unchoke().await;

    // TODO: maybe reset regular unchoke timer
    // choke_interval.reset();
  }

  async fn regular_unchoke(&self) {
    println!("Running regular unchoke algorithm");

    let mut peers = self.peers.lock().await;

    // workaround because can't sort with an async function
    let mut peer_map: HashMap<SocketAddr, u32> = HashMap::new();

    for peer in &*peers {
      let peer_info = peer.peer_info.lock().await;

      let speed = peer_info.download_rate();
      if speed != 0 {
        print!(
          "{}, {}, {} | ",
          speed, peer_info.am_interested, peer_info.peer_choking
        );
      }
      peer_map.insert(peer.address, speed);
    }
    println!();

    // peers are rated by their rolling upload average
    // TODO: when seeding (100% downloaded torrent) they should be rated by their download rate
    peers.sort_by_key(|x| peer_map[&x.address]);
    peers.reverse(); // the best peers should be first

    let mut count = 0;
    // unchoke best peers
    for peer in &mut *peers {
      if let Some(p) = *self.optimistic_unchoke_peer.lock().await {
        // ignore the optimistically unchoked peer
        if p == peer.address {
          continue;
        }
      }

      // if peer_map[&peer.address] == 0 {
      //   // choke bad peers. they will only be unchoked optimistically
      //   peer.send_message(ClientMessage::Choke).await;

      //   continue;
      // }

      if count >= constants::MAX_DOWNLOADERS {
        break;
      }

      if peer.peer_info.lock().await.peer_interested {
        // unchoke
        count += 1;
        peer.send_message(ClientMessage::Unchoke).await;
      } else {
        // Peers which have a better upload rate (as compared to the downloaders) but aren't interested get unchoked.
        // If they become interested, this algorithm is rerun
        peer.send_message(ClientMessage::Unchoke).await;
      }
    }
  }

  async fn choose_first_piece(&self) {
    let mut downloading_piece = self.downloading_piece.lock().await;
    let mut downloading_piece_index = self.downloading_piece_index.lock().await;
    let mut blocks_to_download = self.blocks_to_download.lock().await;

    let piece_index = self
      .downloaded_pieces
      .lock()
      .await
      .iter()
      .position(|x| *x == false);

    if piece_index.is_none() {
      println!("Download is probably done");
      return;
    }

    let piece_index = piece_index.unwrap() as u32;
    println!("Choosing {piece_index} as first piece");

    // get length of selected piece
    let piece_start = piece_index * self.metainfo.piece_length;
    let piece_length = {
      let piece_end = {
        let maybe_end = piece_start + self.metainfo.piece_length;
        let max_end = {
          let last_file = self.files.last().unwrap();
          last_file.offset + last_file.length
        };

        maybe_end.min(max_end)
      };
      piece_end - piece_start
    };

    // div ceil
    let count = (piece_length - 1) / constants::BLOCK_SIZE + 1;

    *downloading_piece = vec![];

    for i in 0..count {
      let block_start = constants::BLOCK_SIZE * i;
      let block_end = (block_start + constants::BLOCK_SIZE).min(piece_length);

      downloading_piece.push(None);
      blocks_to_download.push(BlockInfo {
        index: piece_index,
        begin: block_start,
        length: block_end - block_start,
      });
    }

    // set new piece info as the downloading piece info
    *downloading_piece_index = Some(piece_index);
  }

  pub async fn alert_peers_updated_job_queue(&self) {
    let peers = self.peers.lock().await;
    for peer in &*peers {
      peer.send_message(ClientMessage::UpdatedJobQueue).await;
    }
  }

  async fn try_choose_new_piece(&self) {
    let mut downloading_piece = self.downloading_piece.lock().await;
    let mut downloading_piece_index = self.downloading_piece_index.lock().await;

    // check if the piece, which is currently being downloaded, has completely downloaded
    if downloading_piece.iter().all(|x| x.is_some()) && downloading_piece_index.is_some() {
      let mut blocks_to_download = self.blocks_to_download.lock().await;

      let piece_bytes: Vec<u8> = {
        let mut bytes: Vec<u8> = vec![];
        for block in &mut *downloading_piece {
          bytes.append(&mut block.as_mut().unwrap().bytes);
        }
        bytes
      };

      assert!(blocks_to_download.is_empty()); // TODO ?

      // check hash
      let hashed = sha1_hash(&piece_bytes);
      if hashed != self.metainfo.pieces[downloading_piece_index.unwrap() as usize] {
        // bad piece: some downloaded block was not correct
        println!("Bad piece {}", downloading_piece_index.unwrap());

        // remove all downloaded pieces and move their info to the job queue
        for block in &mut *downloading_piece {
          // TODO: technically we don't need to download every block again
          // all we need to do keep old blocks (one or more of them are wrong)
          // and after downloading a block exchange the old block with the new one and check if hashes match up
          // although, i don't think it's worth it because bad blocks are probably rare (hopefully)
          blocks_to_download.push(block.as_ref().unwrap().info);
          *block = None;
        }
      } else {
        // good piece
        if downloading_piece_index.unwrap() % 100 == 0 {
          println!("Downloaded piece {}", downloading_piece_index.unwrap());
        }

        // write it to disk
        self
          .write_piece(downloading_piece_index.unwrap(), piece_bytes)
          .await;

        // mark it as downloaded
        self.downloaded_pieces.lock().await[downloading_piece_index.unwrap() as usize] = true;

        // send update to peers
        for peer in &*self.peers.lock().await {
          peer
            .send_message(ClientMessage::PieceDownloaded(
              downloading_piece_index.unwrap(),
            ))
            .await;
        }

        // check if all files are downloaded
        if self.downloaded_pieces.lock().await.iter().all(|x| *x) {
          // download fully finished
          println!("Download fully finished");
          *downloading_piece_index = None;
          // TODO: switch to seeding mode
        } else {
          // download not fully finished, some pieces are still left

          // choose next piece
          // TODO: add other methods like rarest first, random
          // pick the next not downloaded piece
          let piece_index = self
            .downloaded_pieces
            .lock()
            .await
            .iter()
            .position(|x| *x == false)
            .unwrap() as u32;

          // get length of selected piece
          let piece_start = piece_index * self.metainfo.piece_length;
          let piece_length = {
            let piece_end = {
              let maybe_end = piece_start + self.metainfo.piece_length;
              let max_end = {
                let last_file = self.files.last().unwrap();
                last_file.offset + last_file.length
              };

              maybe_end.min(max_end)
            };
            piece_end - piece_start
          };

          // div ceil
          let count = (piece_length - 1) / constants::BLOCK_SIZE + 1;

          *downloading_piece = vec![];

          for i in 0..count {
            let block_start = constants::BLOCK_SIZE * i;
            let block_end = (block_start + constants::BLOCK_SIZE).min(piece_length);

            downloading_piece.push(None);
            blocks_to_download.push(BlockInfo {
              index: piece_index,
              begin: block_start,
              length: block_end - block_start,
            });
          }

          // set new piece info as the downloading piece info
          *downloading_piece_index = Some(piece_index);
        }

        self.alert_peers_updated_job_queue().await;
      }
    }
  }

  pub async fn pause(&mut self) -> Result<()> {
    todo!()
  }

  pub async fn get_statistics(&self) -> Result<()> {
    todo!()
  }

  fn get_file_sections(&self, offset: u32, length: u32) -> Vec<(&FileInfo, u32, u32)> {
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
          piece_index * self.metainfo.piece_length,
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

  pub async fn check_whole_hash(&self) -> Vec<bool> {
    let mut v = vec![];

    for i in 0..self.metainfo.pieces.len() {
      v.push(self.check_piece_hash(i as u32).await);
    }

    v
  }

  pub async fn read_block(&self, block_info: &BlockInfo) -> Vec<u8> {
    // TODO: could implement some caching - save the whole piece in memory
    join_all(
      self
        .get_file_sections(
          block_info.index * self.metainfo.piece_length + block_info.begin,
          block_info.length,
        )
        .iter()
        .map(|f| self.read_data(f.0, f.1, f.2)),
    )
    .await
    .concat()
  }

  pub async fn write_piece(&self, piece_index: u32, bytes: Vec<u8>) {
    let sections =
      self.get_file_sections(piece_index * self.metainfo.piece_length, bytes.len() as u32);

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

  async fn write_data(&self, file_info: &FileInfo, offset: u32, bytes: Vec<u8>) {
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

  async fn read_data(&self, file_info: &FileInfo, offset: u32, length: u32) -> Vec<u8> {
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

  async fn bytes_left(&self) -> u32 {
    self
      .downloaded_pieces
      .lock()
      .await
      .iter()
      .filter(|b| **b == false)
      .count() as u32
      * self.metainfo.piece_length
  }

  async fn request_tracker(
    &self,
    event: Option<TrackerEvent>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    // TODO: check if we have saved tracker url

    let mut possible_urls = vec![&self.metainfo.announce];
    if let Some(announce_list) = &self.metainfo.announce_list {
      possible_urls.extend(announce_list.iter());
    }

    for possible_url in possible_urls {
      if let Ok(response) = self.try_request_tracker(possible_url, event).await {
        return Ok(response);
      }
    }

    Err(anyhow!("Couldn't request from any tracker"))
  }

  async fn try_request_tracker(
    &self,
    tracker_url: &str,
    event: Option<TrackerEvent>,
  ) -> Result<TrackerResponse> {
    let protocol = tracker_url.split(':').next();

    match protocol {
      Some("https" | "http") => self.request_tracker_http(tracker_url, event, None).await,
      Some("udp") => self.request_tracker_udp(tracker_url, event, None).await,
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
    tracker_url: &str,
    event: Option<TrackerEvent>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    let mut builder = reqwest::Client::new()
      .get(tracker_url)
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
    tracker_url: &str,
    event: Option<TrackerEvent>,
    tracker_id: Option<String>,
  ) -> Result<TrackerResponse> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;

    let re = Regex::new(r"^udp://(.+):(\d+).*$").unwrap();
    let captures = re
      .captures(tracker_url)
      .ok_or(anyhow!("Invalid announce url"))?;

    let ip_part = captures.get(1).unwrap().as_str().to_string();
    let port_part = u16::from_str_radix(captures.get(2).unwrap().as_str(), 10).unwrap();

    sock.connect((ip_part, port_part)).await?;

    let transaction_id: u32 = random();
    let connection_request: Vec<u8> = {
      let mut buf = vec![];

      let protocol_id: u64 = 0x41727101980;
      buf.extend(protocol_id.to_be_bytes());

      let action: u32 = 0;
      buf.extend(action.to_be_bytes());

      buf.extend(transaction_id.to_be_bytes());

      buf
    };

    // TODO: should retry. same for later calls
    let bytes_sent = sock.send(&connection_request).await?;
    assert_eq!(bytes_sent, connection_request.len());

    let mut connect_response: Vec<u8> = vec![0; 16];
    let bytes_read = sock.recv(&mut connect_response).await?;

    assert!(bytes_read >= connect_response.len());
    assert_eq!(0, u32::from_be_bytes(connect_response[0..4].try_into()?));
    assert_eq!(
      transaction_id,
      u32::from_be_bytes(connect_response[4..8].try_into()?)
    );
    let connection_id = u64::from_be_bytes(connect_response[8..16].try_into()?);

    let transaction_id: u32 = random();
    let announce_request: Vec<u8> = {
      let mut buf = vec![];

      buf.extend(connection_id.to_be_bytes());

      let action: u32 = 1;
      buf.extend(action.to_be_bytes());

      buf.extend(transaction_id.to_be_bytes());

      buf.extend(self.metainfo.info_hash);

      buf.extend(self.peer_id);

      let downloaded: u64 = *self.bytes_downloaded.lock().await as u64;
      buf.extend(downloaded.to_be_bytes());

      let left: u64 = self.bytes_left().await as u64;
      buf.extend(left.to_be_bytes());

      let uploaded: u64 = *self.bytes_uploaded.lock().await as u64;
      buf.extend(uploaded.to_be_bytes());

      let event: u32 = match event {
        None => 0,
        Some(TrackerEvent::Completed) => 1,
        Some(TrackerEvent::Started) => 2,
        Some(TrackerEvent::Stopped) => 3,
      };
      buf.extend(event.to_be_bytes());

      let ip_address: u32 = 0;
      buf.extend(ip_address.to_be_bytes());

      let key: u32 = random(); // no clue what this is for
      buf.extend(key.to_be_bytes());

      let num_want: i32 = -1;
      buf.extend(num_want.to_be_bytes());

      let port: u16 = self.port;
      buf.extend(port.to_be_bytes());

      buf
    };

    let bytes_sent = sock.send(&announce_request).await?;
    assert_eq!(bytes_sent, announce_request.len());

    let mut announce_response: Vec<u8> = vec![0; 20 + 6 * 100];
    let bytes_read = sock.recv(&mut announce_response).await?;

    assert!(bytes_read >= 20);
    assert_eq!(
      transaction_id,
      u32::from_be_bytes(announce_response[4..8].try_into()?)
    );

    let interval = u32::from_be_bytes(announce_response[8..12].try_into()?);
    let leechers = u32::from_be_bytes(announce_response[12..16].try_into()?);
    let seeders = u32::from_be_bytes(announce_response[16..20].try_into()?);

    let mut peers = vec![];

    let peers_count = (bytes_read - 20) / 6;
    for i in 0..peers_count {
      let [a, b, c, d]: [u8; 4] = announce_response[(20 + i * 6)..(20 + i * 6 + 4)].try_into()?;
      let port =
        u16::from_be_bytes(announce_response[(20 + i * 6 + 4)..(20 + i * 6 + 6)].try_into()?);

      peers.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), port));
    }

    Ok(TrackerResponse {
      warning_message: None,
      interval,
      min_interval: None,
      tracker_id,
      complete: leechers,
      incomplete: seeders,
      peer_info: peers,
    })
  }
}
