use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use crate::bytes::{encode_bytes, random_id};
use crate::manager::{FileManager, FileManagerMessage};
use crate::metainfo::{self, MetaInfo};

pub struct Peer {}

#[derive(Debug, Clone)]
pub enum PeerMessage {}

impl PeerMessage {
  pub fn new() -> Self {
    todo!()
  }
}
