use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use crate::bytes::{encode_bytes, random_id};
use crate::metainfo::{self, MetaInfo};

pub struct Peer {
  am_choking: bool,
  am_interested: bool,
  peer_choking: bool,
  peer_interested: bool,
}

#[derive(Debug, Clone)]
pub enum PeerMessage {}

impl Peer {
  pub fn new() -> Self {
    Peer {
      am_choking: true,
      am_interested: false,
      peer_choking: true,
      peer_interested: false,
    }
  }
}
