use anyhow::{anyhow, Result};
use std::{
  net::SocketAddr,
  sync::{Arc, Weak},
  time::Duration,
};

use tokio::{
  io::{AsyncReadExt, AsyncWriteExt},
  net::TcpStream,
  select,
  sync::Mutex,
  time,
};

use crate::{bytes::sha1_hash, constants::MAX_ALLOWED_BLOCK_SIZE, torrent::Torrent};

pub struct Peer {
  address: SocketAddr,
  peer_id: Mutex<Option<[u8; 20]>>, // before handshake it's None

  am_choking: Mutex<bool>,
  am_interested: Mutex<bool>,
  peer_choking: Mutex<bool>,
  peer_interested: Mutex<bool>,

  downloaded_from: Mutex<u64>,
  uploaded_to: Mutex<u64>,
  downloaded_from_rate: Mutex<u64>, // bytes per second
  uploaded_to_rate: Mutex<u64>,     // bytes per second

  piece_availability: Mutex<Vec<bool>>,

  torrent: Weak<Torrent>,
  peer: Weak<Peer>,
}

impl Peer {
  pub fn new(address: SocketAddr, torrent: Weak<Torrent>) -> Arc<Self> {
    Arc::new_cyclic(|me| Peer {
      address,
      peer_id: Mutex::new(None),

      am_choking: Mutex::new(true),
      am_interested: Mutex::new(false),
      peer_choking: Mutex::new(true),
      peer_interested: Mutex::new(false),

      downloaded_from: Mutex::new(0),
      uploaded_to: Mutex::new(0),
      downloaded_from_rate: Mutex::new(0),
      uploaded_to_rate: Mutex::new(0),

      piece_availability: Mutex::new(vec![
        false;
        torrent.clone().upgrade().unwrap().metainfo.pieces.len()
      ]),

      peer: me.clone(),
      torrent,
    })
  }

  pub async fn start(&self, stream: Option<TcpStream>) -> Result<()> {
    let peer = self.peer.upgrade().unwrap();
    let torrent = peer.torrent.upgrade().unwrap();

    tokio::spawn(async move {
      let mut stream = {
        if let Some(mut stream) = stream {
          // receive handshake

          let mut buf = vec![0_u8; 68];

          // TODO: add timeout
          stream.read_exact(&mut buf).await.unwrap();

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

          stream.writable().await.unwrap();
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
          let mut stream = TcpStream::connect(peer.address)
            .await
            .expect("Couldn't connect to peer");

          // send handshake

          stream.writable().await.unwrap();
          stream
            .write_all(&Peer::handshake_message(
              &&torrent.metainfo.info_hash,
              &torrent.peer_id,
            ))
            .await
            .unwrap();

          // receive handshake

          let mut buf = vec![0_u8; 68];

          // TODO: add timeout
          stream.read_exact(&mut buf).await.unwrap();

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

      stream.writable().await.unwrap();
      stream
        .write_all(&Peer::bitfield_message(
          &torrent.downloaded_pieces.lock().await,
        ))
        .await
        .unwrap();

      let mut interval = time::interval(Duration::from_millis(1000));

      //                       index begin length
      let mut requested_pieces: Vec<(u32, u32, u32)> = vec![];

      let mut downloaded_from_last_second = 0;
      let mut uploaded_to_last_second = 0;

      loop {
        select! {
          Ok(_) = stream.readable() => {
            let mut length_buf = [0_u8; 4];

            if let Ok(bytes_read) = stream.peek(&mut length_buf).await {
              // TODO: check if this can happen
              if bytes_read > 4 {
                dbg!("oh");
              }

              if bytes_read == 4 {
                // successfully read first part of the message - length

                let length = u32::from_be_bytes(length_buf);

                let mut buf = vec![0_u8; length as usize + 4];

                // TODO: add timeout
                match stream.read_exact(&mut buf).await {
                  Ok(n) => {
                    let msg_id: u8 = buf[4];

                    match msg_id {
                      0 => {
                        // CHOKE
                        *peer.peer_choking.lock().await = true;
                      }
                      1 => {
                        // UNCHOKE
                        *peer.peer_choking.lock().await = false;
                      }
                      2 => {
                        // INTERESTED
                        *peer.peer_interested.lock().await = true;
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
                            // TODO: disconnect
                            todo!();
                          }

                          *peer.piece_availability.lock().await = bitfield[..actual_len].to_vec();
                      }
                      6 => {
                        // REQUEST

                        if *peer.am_choking.lock().await {
                          continue;
                        }

                        let index = u32::from_be_bytes(buf[5..9].try_into().unwrap());
                        let begin = u32::from_be_bytes(buf[9..13].try_into().unwrap());
                        let length = u32::from_be_bytes(buf[13..17].try_into().unwrap());
                        if length > MAX_ALLOWED_BLOCK_SIZE {
                          todo!();
                        }

                        let chunk = torrent.read_chunk(index, begin as u64, length as u64).await;

                        stream.write_all(&Peer::piece_message(index, begin, &chunk)).await.unwrap();
                      }
                      7 => {
                        // PIECE

                        if *peer.am_choking.lock().await {
                          continue;
                        }

                        let index = u32::from_be_bytes(buf[5..9].try_into().unwrap());
                        let begin = u32::from_be_bytes(buf[9..13].try_into().unwrap());
                        let block = buf[13..].to_vec();

                        if let Some(pos) = requested_pieces.iter().position(|(a, b, c)| *a == index && *b == begin && *c == block.len() as u32) {
                          requested_pieces.remove(pos);

                          // check hash
                          let hashed = sha1_hash(&block);
                          // FIXME: we get a block, not a piece. so need to wait for a whole piece.
                          // Torrent needs to keep track of currently downloading piece.
                          // After a piece is downloaded only then check the hash and if it doesnt match try again

                          // if hashed == torrent.metainfo.pieces[index as usize] {
                          //   // good block
                          //   torrent.write_chunk(index, begin as u64, block).await;
                          // } else {
                          //   // bad block
                          //   // TODO: put back on the queue
                          // }

                          // let chunk = torrent.read_chunk(index, begin as u64, length as u64).await;
                        }
                      }
                      8 => {
                        // CANCEL
                        // there isn't really a use for this?
                      }
                      _ => {
                        // TODO: disconnect client
                        todo!();
                      }
                    }
                  }
                  Err(_) => {
                    // TODO: disconnect client
                    todo!();
                  }
                }
              }
            }
          }

          // update stats every second
          _ = interval.tick() => {
            // calculate rolling peer download/upload speed averages
            let downloaded_from_now = *peer.downloaded_from.lock().await;
            let uploaded_to_now = *peer.uploaded_to.lock().await;

            *peer.downloaded_from_rate.lock().await = downloaded_from_now - downloaded_from_last_second;
            *peer.uploaded_to_rate.lock().await = uploaded_to_now - uploaded_to_last_second;

            downloaded_from_last_second = downloaded_from_now;
            uploaded_to_last_second = uploaded_to_now;
          }
        };
      }
    });

    Ok(())
  }

  pub fn handshake_message(info_hash: &[u8; 20], peer_id: &[u8; 20]) -> Vec<u8> {
    let mut msg = vec![];

    msg.push(19_u8); // pstrlen
    msg.extend(b"BitTorrent protocol"); // pstr
    msg.extend([0_u8; 8]); // reserved
    msg.extend(info_hash); // info_hash
    msg.extend(peer_id); // peer_id

    msg
  }

  pub fn choke_message() -> Vec<u8> {
    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(0_u8); // id

    msg
  }

  pub fn unchoke_message() -> Vec<u8> {
    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(1_u8); // id

    msg
  }

  pub fn interested_message() -> Vec<u8> {
    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(2_u8); // id

    msg
  }

  pub fn not_interested_message() -> Vec<u8> {
    let mut msg = vec![];

    msg.extend(1_u32.to_be_bytes()); // length
    msg.push(3_u8); // id

    msg
  }

  pub fn have_message(piece_index: u32) -> Vec<u8> {
    let mut msg = vec![];

    msg.extend(5_u32.to_be_bytes()); // length
    msg.push(4_u8); // id
    msg.extend(piece_index.to_be_bytes()); // piece_index

    msg
  }

  pub fn bitfield_message(bitfield: &[bool]) -> Vec<u8> {
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
    let mut msg = vec![];

    msg.extend(13_u32.to_be_bytes()); // length
    msg.push(6_u8); // id
    msg.extend(piece_index.to_be_bytes()); // index
    msg.extend(begin.to_be_bytes()); // begin
    msg.extend(length.to_be_bytes()); // length

    msg
  }

  pub fn piece_message(piece_index: u32, begin: u32, block: &[u8]) -> Vec<u8> {
    let mut msg = vec![];

    msg.extend((9_u32 + block.len() as u32).to_be_bytes()); // length
    msg.push(7_u8); // id
    msg.extend(piece_index.to_be_bytes()); // index
    msg.extend(begin.to_be_bytes()); // begin
    msg.extend(block); // block

    msg
  }

  pub fn cancel_message(piece_index: u32, begin: u32, length: u32) -> Vec<u8> {
    let mut msg = vec![];

    msg.extend(13_u32.to_be_bytes()); // length
    msg.push(8_u8); // id
    msg.extend(piece_index.to_be_bytes()); // index
    msg.extend(begin.to_be_bytes()); // begin
    msg.extend(length.to_be_bytes()); // length

    msg
  }
}
