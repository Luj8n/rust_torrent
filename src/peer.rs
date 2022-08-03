use std::net::SocketAddr;

pub struct Peer {
  address: SocketAddr,

  am_choking: bool,
  am_interested: bool,
  peer_choking: bool,
  peer_interested: bool,

  downloaded_from: u64,
  uploaded_to: u64,

  piece_availability: Vec<bool>,
}

#[derive(Debug, Clone)]
pub enum PeerMessage {}

impl Peer {
  pub fn new(address: SocketAddr) -> Self {
    Peer {
      address,

      am_choking: true,
      am_interested: false,
      peer_choking: true,
      peer_interested: false,

      downloaded_from: 0,
      uploaded_to: 0,

      piece_availability: vec![], // TODO
    }
  }

  pub fn start(&self) {
    // TODO: spawn a new tokio task for handling the connection
  }
}
