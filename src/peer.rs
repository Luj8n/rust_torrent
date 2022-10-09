use anyhow::{anyhow, Result};
use std::{
  net::SocketAddr,
  sync::{Arc, Weak},
  time::Duration,
};
use tokio::{
  io::{AsyncReadExt, AsyncWriteExt},
  net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpStream,
  },
  select,
  sync::{
    mpsc::{self, Sender},
    Mutex,
  },
  time::{self, timeout},
};

use crate::{
  constants::{BLOCK_SIZE, MAX_ALLOWED_BLOCK_SIZE, ROLLING_AVERAGE_SIZE, TIMEOUT_DURATION},
  torrent::{Torrent, TorrentMessage},
};

struct PeerMessageReader {
  length_buf: [u8; 4],
  length_buf_len: usize,

  message_buf: Vec<u8>,
  message_buf_len: usize,
}

impl PeerMessageReader {
  fn new() -> Self {
    PeerMessageReader {
      length_buf: [0_u8; 4],
      length_buf_len: 0,
      message_buf: vec![],
      message_buf_len: 0,
    }
  }
  // in general, if a future is dropped, it stops executing at a .await
  // therefore, this function should be cancel safe
  async fn next(&mut self, read_stream: &mut OwnedReadHalf) -> Result<PeerMessage> {
    loop {
      if self.message_buf.len() != 0 {
        let bytes_read = read_stream
          .read(&mut self.message_buf[self.message_buf_len..])
          .await?;
        self.message_buf_len += bytes_read;

        if self.message_buf_len == self.message_buf.len() {
          let peer_message = PeerMessage::from_bytes(&self.message_buf)
            .ok_or(anyhow!("Couldn't parse peer message"))?;

          self.message_buf_len = 0;
          self.message_buf = vec![];

          return Ok(peer_message);
        }

        continue;
      }

      let bytes_read = read_stream
        .read(&mut self.length_buf[self.length_buf_len..])
        .await?;
      self.length_buf_len += bytes_read;

      if self.length_buf_len == 4 {
        self.message_buf_len = 0;
        self.message_buf = vec![0; u32::from_be_bytes(self.length_buf) as usize];

        self.length_buf_len = 0;
        self.length_buf = [0; 4];
      }
    }
  }
}

#[derive(Debug)]
enum PeerMessage {
  Choke,
  Unchoke,
  Interested,
  NotInterested,
  Have(u32),
  Bitfield(Vec<bool>),
  Request(BlockInfo),
  Piece(Block),
  Cancel(BlockInfo),
  // Port(u16), // TODO: DHT tracker
}

impl PeerMessage {
  fn from_bytes(bytes: &[u8]) -> Option<Self> {
    let id = bytes.get(0)?;

    use PeerMessage::*;

    match id {
      0 => Some(Choke),
      1 => Some(Unchoke),
      2 => Some(Interested),
      3 => Some(NotInterested),
      4 => {
        let index = u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?);
        Some(Have(index))
      }
      5 => {
        let bitfield = bytes
          .get(1..)?
          .iter()
          .flat_map(|b| {
            [
              b & 0b10000000 > 0,
              b & 0b01000000 > 0,
              b & 0b00100000 > 0,
              b & 0b00010000 > 0,
              b & 0b00001000 > 0,
              b & 0b00000100 > 0,
              b & 0b00000010 > 0,
              b & 0b00000001 > 0,
            ]
          })
          .collect();

        Some(Bitfield(bitfield))
      }
      6 => {
        let index = u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?);
        let begin = u32::from_be_bytes(bytes.get(5..9)?.try_into().ok()?);
        let length = u32::from_be_bytes(bytes.get(9..13)?.try_into().ok()?);

        let block_info = BlockInfo {
          index,
          begin,
          length,
        };

        Some(Request(block_info))
      }
      7 => {
        let index = u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?);
        let begin = u32::from_be_bytes(bytes.get(5..9)?.try_into().ok()?);
        let block_bytes = bytes.get(9..)?.to_vec();

        let block = Block {
          info: BlockInfo {
            index,
            begin,
            length: block_bytes.len() as u32,
          },
          bytes: block_bytes,
        };

        Some(Piece(block))
      }
      8 => {
        let index = u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?);
        let begin = u32::from_be_bytes(bytes.get(5..9)?.try_into().ok()?);
        let length = u32::from_be_bytes(bytes.get(9..13)?.try_into().ok()?);

        let block_info = BlockInfo {
          index,
          begin,
          length,
        };

        Some(Cancel(block_info))
      }
      _ => None,
    }
  }

  fn as_bytes(&self) -> Vec<u8> {
    use PeerMessage::*;

    let mut msg = vec![];

    match self {
      Choke => {
        msg.extend(1_u32.to_be_bytes()); // length
        msg.push(0_u8); // id

        msg
      }
      Unchoke => {
        msg.extend(1_u32.to_be_bytes()); // length
        msg.push(1_u8); // id

        msg
      }
      Interested => {
        msg.extend(1_u32.to_be_bytes()); // length
        msg.push(2_u8); // id

        msg
      }
      NotInterested => {
        msg.extend(1_u32.to_be_bytes()); // length
        msg.push(3_u8); // id

        msg
      }
      Have(piece_index) => {
        let mut msg = vec![];

        msg.extend(5_u32.to_be_bytes()); // length
        msg.push(4_u8); // id
        msg.extend(piece_index.to_be_bytes()); // piece_index

        msg
      }
      Bitfield(bitfield) => {
        let field: Vec<u8> = bitfield
          .chunks(8)
          .map(|c| {
            ((c.get(0).map_or(0, |x| *x as u8)) << 7)
              + ((c.get(1).map_or(0, |x| *x as u8)) << 6)
              + ((c.get(2).map_or(0, |x| *x as u8)) << 5)
              + ((c.get(3).map_or(0, |x| *x as u8)) << 4)
              + ((c.get(4).map_or(0, |x| *x as u8)) << 3)
              + ((c.get(5).map_or(0, |x| *x as u8)) << 2)
              + ((c.get(6).map_or(0, |x| *x as u8)) << 1)
              + ((c.get(7).map_or(0, |x| *x as u8)) << 0)
          })
          .collect();

        msg.extend((1_u32 + field.len() as u32).to_be_bytes()); // length
        msg.push(5_u8); // id
        msg.extend(field); // bitfield

        msg
      }
      Request(block_info) => {
        msg.extend(13_u32.to_be_bytes()); // length
        msg.push(6_u8); // id
        msg.extend(block_info.index.to_be_bytes()); // index
        msg.extend(block_info.begin.to_be_bytes()); // begin
        msg.extend(block_info.length.to_be_bytes()); // length

        msg
      }
      Piece(block) => {
        msg.extend((9_u32 + block.info.length).to_be_bytes()); // length
        msg.push(7_u8); // id
        msg.extend(block.info.index.to_be_bytes()); // index
        msg.extend(block.info.begin.to_be_bytes()); // begin
        msg.extend(&block.bytes); // block

        msg
      }
      Cancel(block_info) => {
        msg.extend(13_u32.to_be_bytes()); // length
        msg.push(8_u8); // id
        msg.extend(block_info.index.to_be_bytes()); // index
        msg.extend(block_info.begin.to_be_bytes()); // begin
        msg.extend(block_info.length.to_be_bytes()); // length

        msg
      }
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockInfo {
  pub index: u32, // piece index
  pub begin: u32, // offset
  pub length: u32,
}

#[derive(Debug)]
pub struct Block {
  pub info: BlockInfo,
  pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum ClientMessage {
  PieceDownloaded(u32), // index of piece
  Choke,
  Unchoke,
  UpdatedJobQueue,
}

pub struct Peer {
  pub address: SocketAddr,
  peer_id: Mutex<Option<[u8; 20]>>, // before handshake it's None

  pub am_choking: Mutex<bool>,
  pub am_interested: Mutex<bool>,
  pub peer_choking: Mutex<bool>,
  pub peer_interested: Mutex<bool>,

  downloaded_from: Mutex<u32>, // total downloaded bytes
  uploaded_to: Mutex<u32>,     // total uploaded bytes

  rolling_download: Mutex<Vec<u32>>,
  rolling_upload: Mutex<Vec<u32>>,

  pub downloaded_from_rate: Mutex<u32>, // rolling average, updated every second
  pub uploaded_to_rate: Mutex<u32>,     // rolling average, updated every second

  queue_size: Mutex<usize>,

  pub piece_availability: Mutex<Vec<bool>>,
  requested_blocks: Mutex<Vec<BlockInfo>>,

  torrent: Weak<Torrent>,
  torrent_sender: Sender<TorrentMessage>,
  peer_sender: Mutex<Option<Sender<ClientMessage>>>,
  peer: Weak<Peer>,
}

#[derive(Debug)]
enum PeerWriterMessage {
  SendChoke,
  SendUnchoke,
  SendInterested,
  SendNotInterested,
  SendHave(u32),
  SendBitfield,
  SendRequest(BlockInfo),
  SendBlock(BlockInfo),
  SendCancel(BlockInfo),
}

impl Peer {
  pub fn new(
    address: SocketAddr,
    torrent: Weak<Torrent>,
    torrent_sender: Sender<TorrentMessage>,
  ) -> Arc<Self> {
    Arc::new_cyclic(|me| Peer {
      address,
      peer_id: Mutex::new(None),

      am_choking: Mutex::new(true),
      am_interested: Mutex::new(false),
      peer_choking: Mutex::new(true),
      peer_interested: Mutex::new(false),

      downloaded_from: Mutex::new(0),
      uploaded_to: Mutex::new(0),

      rolling_download: Mutex::new(vec![0]),
      rolling_upload: Mutex::new(vec![0]),

      downloaded_from_rate: Mutex::new(0),
      uploaded_to_rate: Mutex::new(0),

      queue_size: Mutex::new(1),

      piece_availability: Mutex::new(vec![
        false;
        torrent.clone().upgrade().unwrap().metainfo.pieces.len()
      ]),
      requested_blocks: Mutex::new(vec![]),

      torrent,
      torrent_sender,
      peer_sender: Mutex::new(None),
      peer: me.clone(),
    })
  }

  pub async fn send_message(&self, peer_message: ClientMessage) {
    if let Some(sender) = &*self.peer_sender.lock().await {
      match sender.send(peer_message).await {
        Ok(_) => {}
        Err(_) => println!("Couldn't send message to peer"),
      };
    }
  }

  pub fn start(&self, stream: Option<TcpStream>) -> Result<()> {
    let peer = self
      .peer
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade peer"))?;

    let torrent = peer
      .torrent
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade torrent"))?;

    tokio::spawn(async move {
      match peer.start_handler(stream).await {
        Ok(_) => println!("Peer has disconnected"),
        Err(e) => println!(
          "Peer has disconnected by crashing. Error: {}",
          e.to_string()
        ),
      };

      // disconnect peer
      torrent.disconnect_peer(peer.address).await;
    });

    Ok(())
  }

  async fn try_update_job_queue(
    peer: Arc<Peer>,
    torrent: Arc<Torrent>,
    peer_writer_sender: &Sender<PeerWriterMessage>,
  ) -> Result<()> {
    let queue_size = peer.queue_size.lock().await;
    let mut requested_blocks = peer.requested_blocks.lock().await;
    let peer_choking = peer.peer_choking.lock().await;
    let piece_availability = peer.piece_availability.lock().await;
    let mut blocks_to_download = torrent.blocks_to_download.lock().await;

    // don't request new blocks if job queue is full, we are being choked or there are no blocks to download
    if requested_blocks.len() >= *queue_size || *peer_choking || blocks_to_download.is_empty() {
      return Ok(());
    }

    while requested_blocks.len() < *queue_size && !blocks_to_download.is_empty() {
      for i in 0..blocks_to_download.len() {
        if piece_availability[blocks_to_download[i].index as usize] {
          let block_info = blocks_to_download.remove(i);
          // send request
          requested_blocks.push(block_info);

          peer_writer_sender
            .send(PeerWriterMessage::SendRequest(block_info))
            .await?;

          let peer = peer.clone();
          let torrent = torrent.clone();

          // after some time check if the block was downloaded
          Peer::create_request_timeout(peer, torrent, block_info);
          break;
        }
      }
    }

    Ok(())
  }

  fn create_request_timeout(peer: Arc<Peer>, torrent: Arc<Torrent>, block_info: BlockInfo) {
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_secs(7)).await;

      let mut requested_blocks = peer.requested_blocks.lock().await;

      if let Some(pos) = requested_blocks.iter().position(|x| *x == block_info) {
        requested_blocks.remove(pos);

        torrent.blocks_to_download.lock().await.push(block_info);

        torrent.alert_peers_updated_job_queue().await;
      }
    });
  }

  async fn receive_handshake(&self, stream: &mut TcpStream) -> Result<()> {
    println!("Receiving handshake");

    let mut buf = vec![0_u8; 68];

    timeout(TIMEOUT_DURATION, stream.read_exact(&mut buf)).await??;

    let pstrlen = buf[0];
    let pstr = &buf[1..20];
    // let reserved = &buf[20..28];
    let info_hash = &buf[28..48];
    let peer_id = &buf[48..68];

    if pstrlen != 19
      || pstr != b"BitTorrent protocol"
      || info_hash
        != self
          .torrent
          .upgrade()
          .ok_or(anyhow!("Couldn't upgrade torrent"))?
          .metainfo
          .info_hash
    {
      return Err(anyhow!("Bad handshake"));
    }

    *self.peer_id.lock().await = Some(peer_id.try_into()?);

    Ok(())
  }

  async fn send_handshake(&self, stream: &mut TcpStream) -> Result<()> {
    println!("Sending handshake");

    let torrent = self
      .torrent
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade torrent"))?;

    let mut bytes = vec![];

    bytes.push(19_u8); // pstrlen
    bytes.extend(b"BitTorrent protocol"); // pstr
    bytes.extend([0_u8; 8]); // reserved
    bytes.extend(&torrent.metainfo.info_hash); // info_hash
    bytes.extend(&torrent.peer_id); // peer_id

    stream.write_all(&bytes).await?;

    Ok(())
  }

  fn start_writer(&self, mut write_stream: OwnedWriteHalf) -> Result<Sender<PeerWriterMessage>> {
    let peer = self
      .peer
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade peer"))?;

    let torrent = peer
      .torrent
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade torrent"))?;

    let (tx, mut rx) = mpsc::channel::<PeerWriterMessage>(10);

    tokio::spawn(async move {
      while let Some(message) = rx.recv().await {
        use PeerWriterMessage::*;
        match message {
          SendChoke => {
            write_stream
              .write_all(&PeerMessage::Choke.as_bytes())
              .await?;
          }
          SendUnchoke => {
            write_stream
              .write_all(&PeerMessage::Unchoke.as_bytes())
              .await?;
          }
          SendInterested => {
            write_stream
              .write_all(&PeerMessage::Interested.as_bytes())
              .await?;
          }
          SendNotInterested => {
            write_stream
              .write_all(&PeerMessage::NotInterested.as_bytes())
              .await?;
          }
          SendHave(piece_index) => {
            write_stream
              .write_all(&PeerMessage::Have(piece_index).as_bytes())
              .await?;
          }
          SendBitfield => {
            write_stream
              .write_all(
                &PeerMessage::Bitfield(torrent.downloaded_pieces.lock().await.clone()).as_bytes(),
              )
              .await?;
          }
          SendRequest(block_info) => {
            write_stream
              .write_all(&PeerMessage::Request(block_info).as_bytes())
              .await?;
          }
          SendBlock(block_info) => {
            let block_bytes = torrent.read_block(&block_info).await;

            assert_eq!(block_info.length as usize, block_bytes.len());

            let block = Block {
              info: block_info,
              bytes: block_bytes,
            };

            write_stream
              .write_all(&PeerMessage::Piece(block).as_bytes())
              .await?;
          }
          SendCancel(block_info) => {
            // TODO
          }
        }
      }

      anyhow::Ok(())
    });

    Ok(tx)
  }

  async fn start_connection(&self, stream: Option<TcpStream>) -> Result<TcpStream> {
    if let Some(mut stream) = stream {
      self.receive_handshake(&mut stream).await?;

      self.send_handshake(&mut stream).await?;

      Ok(stream)
    } else {
      let mut stream = timeout(TIMEOUT_DURATION, TcpStream::connect(self.address)).await??;

      self.send_handshake(&mut stream).await?;

      self.receive_handshake(&mut stream).await?;

      Ok(stream)
    }
  }

  async fn check_if_interested(
    peer: Arc<Peer>,
    torrent: Arc<Torrent>,
    peer_writer_sender: &Sender<PeerWriterMessage>,
  ) -> Result<()> {
    let interested = {
      let downloaded_pieces = torrent.downloaded_pieces.lock().await;
      let peer_pieces = peer.piece_availability.lock().await;

      assert_eq!(downloaded_pieces.len(), peer_pieces.len());

      downloaded_pieces
        .iter()
        .zip(peer_pieces.iter())
        .any(|(our, peers)| *our && *peers)
    };

    if interested != *peer.am_interested.lock().await {
      *peer.am_interested.lock().await = interested;

      if interested {
        println!("Interested in {}", peer.address);

        peer_writer_sender
          .send(PeerWriterMessage::SendInterested)
          .await?;
      } else {
        println!("Not interested in {}", peer.address);

        peer_writer_sender
          .send(PeerWriterMessage::SendNotInterested)
          .await?;
      }

      Peer::try_update_job_queue(peer, torrent, peer_writer_sender).await?;
    }

    Ok(())
  }

  async fn start_handler(&self, stream: Option<TcpStream>) -> Result<()> {
    let peer = self
      .peer
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade peer"))?;

    let torrent = peer
      .torrent
      .upgrade()
      .ok_or(anyhow!("Couldn't upgrade torrent"))?;

    let (tx, mut rx) = mpsc::channel::<ClientMessage>(10);

    *peer.peer_sender.lock().await = Some(tx);

    let stream = peer.start_connection(stream).await?;

    let (mut read_stream, write_stream) = stream.into_split();
    let peer_writer_sender = peer.start_writer(write_stream)?;

    peer_writer_sender
      .send(PeerWriterMessage::SendBitfield)
      .await?;

    let mut one_sec_interval = time::interval(Duration::from_millis(1000));

    let mut downloaded_from_last_second = 0;
    let mut uploaded_to_last_second = 0;

    let mut in_slow_start = true;

    let mut peer_message_reader = PeerMessageReader::new();

    'main_loop: loop {
      select! {
        Some(msg) = rx.recv() => {
          use ClientMessage::*;
          match msg {
            PieceDownloaded(index) => {
              // send HAVE message
              peer_writer_sender.send(PeerWriterMessage::SendHave(index)).await?;

              Peer::check_if_interested(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
            }
            Choke => {
              // choke if we aren't choking
              if !*peer.am_choking.lock().await {
                println!("Choking {}", peer.address);

                *peer.am_choking.lock().await = true;

                // send CHOKE message
                peer_writer_sender.send(PeerWriterMessage::SendChoke).await?;
              }
            }
            Unchoke => {
              // unchoke if we are choking
              if *peer.am_choking.lock().await {
                println!("Unchoking {}", peer.address);

                *peer.am_choking.lock().await = false;

                // send UNCHOKE message
                peer_writer_sender.send(PeerWriterMessage::SendUnchoke).await?;
              }
            }
            UpdatedJobQueue => {
              Peer::try_update_job_queue(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
            }
          }
        }

        Ok(peer_message) = peer_message_reader.next(&mut read_stream) => {
          use PeerMessage::*;
          match peer_message {
            Choke => {
              *peer.peer_choking.lock().await = true;

              Peer::try_update_job_queue(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
            }
            Unchoke => {
              *peer.peer_choking.lock().await = false;

              Peer::try_update_job_queue(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
            }
            Interested => {
              *peer.peer_interested.lock().await = true;

              torrent.interested_peer().await;
            }
            NotInterested => {
              *peer.peer_interested.lock().await = false;
            }
            Have(piece_index) => {
              *peer
                .piece_availability
                .lock()
                .await
                .get_mut(piece_index as usize)
                .ok_or(anyhow!("Bad piece_index"))? = true;

              Peer::check_if_interested(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
            }
            Bitfield(bitfield) => {
              let actual_len = torrent.metainfo.pieces.len();

              if actual_len < peer.piece_availability.lock().await.len() {
                // disconnect client
                break 'main_loop;
              }

              println!("Got bitfield");

              *peer.piece_availability.lock().await = bitfield[..actual_len].to_vec();

              Peer::check_if_interested(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
            }
            Request(block_info) => {
              if *peer.am_choking.lock().await {
                // don't upload to choked peers
                continue;
              }

              if block_info.length > MAX_ALLOWED_BLOCK_SIZE {
                // disconnect
                break 'main_loop;
              }

              peer_writer_sender.send(PeerWriterMessage::SendBlock(block_info)).await?;

              // update bytes uploaded
              *peer.uploaded_to.lock().await += block_info.length;
            }
            Piece(block) => {
              let mut requested_blocks = peer.requested_blocks.lock().await;

              if let Some(pos) = requested_blocks.iter().position(|data| *data == block.info) {
                // only requested pieces are accepted
                requested_blocks.remove(pos);
                drop(requested_blocks);

                // update bytes downloaded
                *peer.downloaded_from.lock().await += block.info.length;

                torrent.block_downloaded(block).await;

                if in_slow_start {
                  // check if the downloaded bytes in the last second is worse than the rolling average
                  let last_download_delta = peer.rolling_download.lock().await[0];
                  let average_download = *peer.downloaded_from_rate.lock().await;

                  let mut queue_size = self.queue_size.lock().await;
                  if last_download_delta + 10000 < average_download {
                    println!("Stopping slow start");

                    in_slow_start = false;

                    if *queue_size > 1 {
                      *queue_size -= 1;
                    }
                  } else {
                    *queue_size += 1;
                  }
                }

                Peer::try_update_job_queue(peer.clone(), torrent.clone(), &peer_writer_sender).await?;
              }
            }
            Cancel(block_info) => {
              // TODO: find out what to do with cancel
            }
          }
        }

        // update stats every second
        _ = one_sec_interval.tick() => {
          // calculate rolling peer download/upload speed averages
          let downloaded_from_now = *peer.downloaded_from.lock().await;
          let uploaded_to_now = *peer.uploaded_to.lock().await;

          let download_delta = downloaded_from_now - downloaded_from_last_second;
          let upload_delta = uploaded_to_now - uploaded_to_last_second;

          let mut rolling_download = peer.rolling_download.lock().await;
          let mut rolling_upload = peer.rolling_upload.lock().await;

          rolling_download.insert(0, download_delta);
          rolling_upload.insert(0, upload_delta);

          rolling_download.truncate(ROLLING_AVERAGE_SIZE as usize);
          rolling_upload.truncate(ROLLING_AVERAGE_SIZE as usize);

          let downloaded_from_rate = rolling_download.iter().sum::<u32>() / rolling_download.len() as u32;
          let uploaded_to_rate = rolling_upload.iter().sum::<u32>() / rolling_upload.len() as u32;

          *peer.downloaded_from_rate.lock().await = downloaded_from_rate;
          *peer.uploaded_to_rate.lock().await = uploaded_to_rate;

          // don't change the queue size if we are choked (= not downloading) or if we are in slow start
          if !*peer.peer_choking.lock().await && !in_slow_start {
            let new_queue_size = (downloaded_from_rate / BLOCK_SIZE).max(1) as usize;

            *peer.queue_size.lock().await = new_queue_size;

          }

          if !*peer.peer_choking.lock().await || !*peer.am_choking.lock().await {
            println!();
            println!("----------------------------");
            println!("Peer address: {}", peer.address);
            println!("Download rate: {} Mb/s", downloaded_from_rate / (1024 * 1024));
            println!("Upload rate: {} Mb/s", uploaded_to_rate / (1024 * 1024));
            println!("Queue size: {}", *peer.queue_size.lock().await);
            println!("----------------------------");
          }

          downloaded_from_last_second = downloaded_from_now;
          uploaded_to_last_second = uploaded_to_now;
        }
      };
    }

    Ok(())
  }
}
