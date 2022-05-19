use anyhow::{anyhow, Result};
use bip_bencode::{BDecodeOpt, BRefAccess, BencodeRef};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use crate::bytes::{encode_bytes, random_id};
use crate::metainfo::MetaInfo;
use crate::torrent::Torrent;

pub struct TorrentManager {
  torrents: Vec<Torrent>,
}
