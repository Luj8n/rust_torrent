use anyhow::Result;
use std::{
  net::SocketAddr,
  sync::{Arc, Weak},
  time::Duration,
};
use tokio::{
  io::{AsyncReadExt, AsyncWriteExt},
  net::TcpStream,
  select,
  sync::{mpsc::Sender, Mutex},
  time,
};

use crate::{
  constants::{self, MAX_ALLOWED_BLOCK_SIZE},
  torrent::{Torrent, TorrentMessage},
};

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
pub enum PeerMessage {
  PieceDownloaded(u32), // index of piece
  Choke,
  Unchoke,
  RequestPiece(BlockInfo),
  UpdatedJobQueue,
}

// TODO: add trait Chunk and methods for it

// FIXME: select in a loop is not working correctly. interval branch is not being executed.

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

  pub piece_availability: Mutex<Vec<bool>>,
  requested_blocks: Mutex<Vec<BlockInfo>>,

  torrent: Weak<Torrent>,
  torrent_sender: Sender<TorrentMessage>,
  peer_sender: Mutex<Option<Sender<PeerMessage>>>,
  peer: Weak<Peer>,
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

      rolling_download: Mutex::new(vec![]),
      rolling_upload: Mutex::new(vec![]),

      downloaded_from_rate: Mutex::new(0),
      uploaded_to_rate: Mutex::new(0),

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

  pub async fn send_message(&self, peer_message: PeerMessage) {
    if let Some(sender) = &*self.peer_sender.lock().await {
      match sender.send(peer_message).await {
        Ok(_) => {}
        Err(_) => println!("Couldn't send message to peer"),
      };
    }
  }

  pub fn start(&self, stream: Option<TcpStream>) -> Result<()> {
    let peer = self.peer.upgrade().unwrap();
    let torrent = peer.torrent.upgrade().unwrap();

    tokio::spawn(async move {
      match Peer::start_handler(peer.clone(), torrent.clone(), stream).await {
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

  async fn try_update_job_queue(peer: Arc<Peer>, torrent: Arc<Torrent>) {
    println!("Trying to update job queue");

    // TODO: make the queue size dynamic
    let queue_size = 3;

    // don't request new blocks if job queue is full, we are being choked or there are no blocks to download
    println!(
      "Requested blocks = {}, peer is choking us = {}, there is no blocks to download = {}",
      peer.requested_blocks.lock().await.len(),
      peer.peer_choking.lock().await,
      torrent.blocks_to_download.lock().await.is_empty()
    );

    if peer.requested_blocks.lock().await.len() >= queue_size
      || *peer.peer_choking.lock().await
      || torrent.blocks_to_download.lock().await.is_empty()
    {
      return;
    }

    println!("Trying to find block to request");

    let piece_availability = peer.piece_availability.lock().await;
    let mut blocks_to_download = torrent.blocks_to_download.lock().await;

    for i in 0..blocks_to_download.len() {
      if piece_availability[blocks_to_download[i].index as usize] {
        let block_info = blocks_to_download.remove(i);
        // send request
        peer.requested_blocks.lock().await.push(block_info);

        peer
          .send_message(PeerMessage::RequestPiece(block_info))
          .await;

        let peer = peer.clone();
        let torrent = torrent.clone();

        // after some time check if the block was downloaded
        Peer::create_request_timeout(peer, torrent, block_info);
        break;
      }
    }
  }

  fn create_request_timeout(peer: Arc<Peer>, torrent: Arc<Torrent>, block_info: BlockInfo) {
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_secs(5)).await;

      let mut requested_blocks = peer.requested_blocks.lock().await;

      if let Some(pos) = requested_blocks.iter().position(|x| *x == block_info) {
        requested_blocks.remove(pos);

        torrent.blocks_to_download.lock().await.push(block_info);

        torrent.alert_peers_updated_job_queue().await;
      }
    });
  }

  async fn start_handler(
    peer: Arc<Peer>,
    torrent: Arc<Torrent>,
    stream: Option<TcpStream>,
  ) -> Result<()> {
    println!("Added new peer");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<PeerMessage>(32);

    *peer.peer_sender.lock().await = Some(tx);

    // TODO: replace most unwraps/expects with "?"

    let mut stream = {
      if let Some(mut stream) = stream {
        // receive handshake
        println!("Receiving handshake");

        let mut buf = vec![0_u8; 68];

        // TODO: add timeout
        stream.read_exact(&mut buf).await?;

        let pstrlen = buf[0];
        let pstr = &buf[1..20];
        // let reserved = &buf[20..28];
        let info_hash = &buf[28..48];
        let peer_id = &buf[48..68];

        assert_eq!(pstrlen, 19);
        assert_eq!(pstr, b"BitTorrent protocol");
        assert_eq!(info_hash, torrent.metainfo.info_hash);
        *peer.peer_id.lock().await = Some(peer_id.try_into().unwrap());

        // send handshake
        println!("Sending handshake");

        stream
          .write_all(&Peer::handshake_message(
            &torrent.metainfo.info_hash,
            &torrent.peer_id,
          ))
          .await
          .unwrap();

        stream
      } else {
        // TODO: add timeout
        dbg!(peer.address);
        let mut stream = TcpStream::connect(peer.address).await?;

        // send handshake
        println!("Sending handshake");

        stream
          .write_all(&Peer::handshake_message(
            &torrent.metainfo.info_hash,
            &torrent.peer_id,
          ))
          .await?;

        // receive handshake
        println!("Receiving handshake");

        let mut buf = vec![0_u8; 68];

        // TODO: add timeout

        stream.read_exact(&mut buf).await?;

        let pstrlen = buf[0];
        let pstr = &buf[1..20];
        // let reserved = &buf[20..28];
        let info_hash = &buf[28..48];
        let peer_id = &buf[48..68];

        assert_eq!(pstrlen, 19);
        assert_eq!(pstr, b"BitTorrent protocol");
        assert_eq!(info_hash, torrent.metainfo.info_hash);
        *peer.peer_id.lock().await = Some(peer_id.try_into().unwrap());

        stream
      }
    };

    // send bitfield
    println!("Sending bitfield");

    stream
      .write_all(&Peer::bitfield_message(
        &torrent.downloaded_pieces.lock().await,
      ))
      .await
      .unwrap();

    let mut one_sec_interval = time::interval(Duration::from_millis(1000));

    let mut downloaded_from_last_second = 0;
    let mut uploaded_to_last_second = 0;

    'main_loop: loop {
      select! {
        // listen to the torrent. receive piece updates (send "HAVE" message, check if interested)
        Some(msg) = rx.recv() => {
          use PeerMessage::*;
          match msg {
            PieceDownloaded(index) => {
              // send HAVE message
              stream.write_all(&Peer::have_message(index)).await.unwrap();

              let downloaded_pieces = torrent.downloaded_pieces.lock().await;
              let peer_pieces = peer.piece_availability.lock().await;

              assert_eq!(downloaded_pieces.len(), peer_pieces.len());
              let mut interested = false;
              for i in 0..downloaded_pieces.len() {
                if !downloaded_pieces[i] && peer_pieces[i] {
                  // peer has a piece that we dont have. therefore we are interested
                  interested = true;
                  break;
                }
              }

              if interested != *peer.am_interested.lock().await {
                *peer.am_interested.lock().await = interested;

                if interested {
                  println!("Interested in {}", peer.address);

                  stream.write_all(&Peer::interested_message()).await.unwrap();
                } else {
                  println!("Not interested in {}", peer.address);

                  stream.write_all(&Peer::not_interested_message()).await.unwrap();
                }
              }

              Peer::try_update_job_queue(peer.clone(), torrent.clone()).await;
            }
            Choke => {
              // choke if we aren't choking
              if !*peer.am_choking.lock().await {
                println!("Choking {}", peer.address);

                *peer.am_choking.lock().await = true;

                // send CHOKE message
                stream.write_all(&Peer::choke_message()).await.unwrap();
              }
            }
            Unchoke => {
              // unchoke if we are choking
              if *peer.am_choking.lock().await {
                println!("Unchoking {}", peer.address);

                *peer.am_choking.lock().await = false;

                // send UNCHOKE message
                stream.write_all(&Peer::unchoke_message()).await.unwrap();
              }
            }
            RequestPiece(block_info) => {
              println!("Requesting block of piece n.{} from {}", block_info.index, peer.address);

              stream.write_all(&Peer::request_message(
                block_info.index,
                block_info.begin,
                block_info.length,
              )).await.unwrap();
            }
            UpdatedJobQueue => {
              Peer::try_update_job_queue(peer.clone(), torrent.clone()).await;
            }
          }
        }

        Ok(_) = stream.readable() => {
          let mut length_buf = [0_u8; 4];

          if let Ok(bytes_read) = stream.peek(&mut length_buf).await {
            // TODO: check if this can happen
            if bytes_read > 4 {
              dbg!("oh");
            }


            if bytes_read == 4 {
              println!("Peer received a message");

              // successfully read first part of the message - length

              let length = u32::from_be_bytes(length_buf);

              let mut buf = vec![0_u8; length as usize + 4];

              // TODO: add timeout
              match stream.read_exact(&mut buf).await {
                Ok(_) => {
                  let msg_id: u8 = buf[4];

                  match msg_id {
                    0 => {
                      // CHOKE
                      *peer.peer_choking.lock().await = true;

                      Peer::try_update_job_queue(peer.clone(), torrent.clone()).await;
                    }
                    1 => {
                      // UNCHOKE
                      *peer.peer_choking.lock().await = false;

                      Peer::try_update_job_queue(peer.clone(), torrent.clone()).await;
                    }
                    2 => {
                      // INTERESTED
                      *peer.peer_interested.lock().await = true;

                      // TODO: maybe don't send message
                      peer
                        .torrent_sender
                        .send(TorrentMessage::InterestedPeer(peer.address))
                        .await
                        .unwrap();
                    }
                    3 => {
                      // NOT_INTERESTED
                      *peer.peer_interested.lock().await = false;
                    }
                    4 => {
                      // HAVE
                      let piece_index = u32::from_be_bytes(buf[5..9].try_into().unwrap());

                      *peer
                        .piece_availability
                        .lock()
                        .await
                        .get_mut(piece_index as usize)
                        .expect("TODO") = true;

                      // check if we are interested now
                      let downloaded_pieces = torrent.downloaded_pieces.lock().await;
                      let peer_pieces = peer.piece_availability.lock().await;

                      assert_eq!(downloaded_pieces.len(), peer_pieces.len());
                      let mut interested = false;
                      for i in 0..downloaded_pieces.len() {
                        if !downloaded_pieces[i] && peer_pieces[i] {
                          // peer has a piece that we dont have. therefore we are interested
                          interested = true;
                          break;
                        }
                      }

                      if interested != *peer.am_interested.lock().await {
                        *peer.am_interested.lock().await = interested;

                        if interested {
                          // send INTERESTED message
                          stream.write_all(&Peer::interested_message()).await.unwrap();
                        } else {
                          // send NOT_INTERESTED message
                          stream.write_all(&Peer::not_interested_message()).await.unwrap();
                        }
                      }
                    }
                    5 => {
                      // BITFIELD

                      let bitfield: Vec<bool> = buf[5..].iter().flat_map(|b|
                        [
                          b & 0b10000000 > 0,
                          b & 0b01000000 > 0,
                          b & 0b00100000 > 0,
                          b & 0b00010000 > 0,
                          b & 0b00001000 > 0,
                          b & 0b00000100 > 0,
                          b & 0b00000010 > 0,
                          b & 0b00000001 > 0,
                        ]).collect();

                        let actual_len = torrent.metainfo.pieces.len();

                        if actual_len < peer.piece_availability.lock().await.len() {
                          // disconnect client
                          break 'main_loop;
                        }

                        *peer.piece_availability.lock().await = bitfield[..actual_len].to_vec();
                    }
                    6 => {
                      // REQUEST

                      if *peer.am_choking.lock().await {
                        // don't upload to choked peers
                        continue;
                      }

                      let index = u32::from_be_bytes(buf[5..9].try_into().unwrap());
                      let begin = u32::from_be_bytes(buf[9..13].try_into().unwrap());
                      let length = u32::from_be_bytes(buf[13..17].try_into().unwrap());
                      if length > MAX_ALLOWED_BLOCK_SIZE {
                        // disconnect
                        break 'main_loop;
                      }

                      // i don't think it actually means to send it immediately, just when you get it. that's probably why "CANCEL" exists
                      // TODO: find out how it actually should be

                      let chunk = torrent.read_block(&BlockInfo { index, begin, length }).await;

                      stream.write_all(&Peer::piece_message(index, begin, &chunk)).await.unwrap();

                      // update bytes uploaded
                      *peer.uploaded_to.lock().await += length;
                    }
                    7 => {
                      // PIECE

                      if *peer.am_choking.lock().await {
                        continue;
                      }

                      let index = u32::from_be_bytes(buf[5..9].try_into().unwrap());
                      let begin = u32::from_be_bytes(buf[9..13].try_into().unwrap());
                      let block = buf[13..].to_vec();

                      let sent_block = Block {
                        info: BlockInfo {
                          index,
                          begin,
                          length: block.len() as u32,
                        },
                        bytes: block,
                      };

                      let mut requested_blocks = peer.requested_blocks.lock().await;

                      if let Some(pos) = requested_blocks.iter().position(|data| *data == sent_block.info) {
                        // only requested pieces are accepted
                        requested_blocks.remove(pos);

                        // update bytes downloaded
                        *peer.downloaded_from.lock().await += sent_block.info.length;

                        peer.torrent_sender.send(TorrentMessage::BlockDownloaded(sent_block)).await.unwrap();

                        Peer::try_update_job_queue(peer.clone(), torrent.clone()).await;
                      }
                    }
                    8 => {
                      // CANCEL
                      // there isn't really a use for this?
                      // TODO
                    }
                    _ => {
                      // disconnect client
                      break 'main_loop;

                    }
                  }
                }
                Err(_) => {
                  // disconnect client
                  break 'main_loop;
                }
              }
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

          rolling_download.truncate(constants::ROLLING_AVERAGE_SIZE as usize);
          rolling_upload.truncate(constants::ROLLING_AVERAGE_SIZE as usize);

          *peer.downloaded_from_rate.lock().await = rolling_download.iter().sum::<u32>() / rolling_download.len() as u32;
          *peer.uploaded_to_rate.lock().await = rolling_upload.iter().sum::<u32>() / rolling_upload.len() as u32;

          println!("Download rate: {}", *peer.downloaded_from_rate.lock().await / (1024 * 1024));
          println!("Upload rate: {}", *peer.uploaded_to_rate.lock().await / (1024 * 1024));

          downloaded_from_last_second = downloaded_from_now;
          uploaded_to_last_second = uploaded_to_now;
        }
      };
    }

    Ok(())
  }

  pub fn handshake_message(info_hash: &[u8; 20], peer_id: &[u8; 20]) -> Vec<u8> {
    let mut msg = vec![];

    msg.push(19_u8); // pstrlen
    msg.extend(b"BitTorrent protocol"); // pstr
    msg.extend([0_u8; 8]); // reserved
    msg.extend(info_hash); // info_hash
    msg.extend(peer_id); // peer_id

    assert_eq!(msg.len(), 68);

    msg
  }

  pub fn choke_message() -> Vec<u8> {
    println!("Sending choke_message");

    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(0_u8); // id

    msg
  }

  pub fn unchoke_message() -> Vec<u8> {
    println!("Sending unchoke_message");

    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(1_u8); // id

    msg
  }

  pub fn interested_message() -> Vec<u8> {
    println!("Sending interested_message");

    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(2_u8); // id

    msg
  }

  pub fn not_interested_message() -> Vec<u8> {
    println!("Sending not_interested_message");

    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(3_u8); // id

    msg
  }

  pub fn have_message(piece_index: u32) -> Vec<u8> {
    println!("Sending have_message");

    let mut msg = vec![];

    msg.extend(5_u32.to_be_bytes()); // length
    msg.push(4_u8); // id
    msg.extend(piece_index.to_be_bytes()); // piece_index

    msg
  }

  pub fn bitfield_message(bitfield: &[bool]) -> Vec<u8> {
    println!("Sending bitfield_message");

    let mut msg = vec![];

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

  pub fn request_message(piece_index: u32, begin: u32, length: u32) -> Vec<u8> {
    println!("Sending request_message");

    let mut msg = vec![];

    msg.extend(13_u32.to_be_bytes()); // length
    msg.push(6_u8); // id
    msg.extend(piece_index.to_be_bytes()); // index
    msg.extend(begin.to_be_bytes()); // begin
    msg.extend(length.to_be_bytes()); // length

    msg
  }

  pub fn piece_message(piece_index: u32, begin: u32, block: &[u8]) -> Vec<u8> {
    println!("Sending piece_message");

    let mut msg = vec![];

    msg.extend((9_u32 + block.len() as u32).to_be_bytes()); // length
    msg.push(7_u8); // id
    msg.extend(piece_index.to_be_bytes()); // index
    msg.extend(begin.to_be_bytes()); // begin
    msg.extend(block); // block

    msg
  }

  // TODO: find out what to do with this
  pub fn cancel_message(piece_index: u32, begin: u32, length: u32) -> Vec<u8> {
    println!("Sending cancel_message");

    let mut msg = vec![];

    msg.extend(13_u32.to_be_bytes()); // length
    msg.push(8_u8); // id
    msg.extend(piece_index.to_be_bytes()); // index
    msg.extend(begin.to_be_bytes()); // begin
    msg.extend(length.to_be_bytes()); // length

    msg
  }
}
