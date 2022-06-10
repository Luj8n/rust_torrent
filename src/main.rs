#![feature(array_chunks)]

use std::path::Path;
use torrent::Torrent;

use crate::manager::TorrentManager;
// use torrent_search::{search_l337x, TorrentSearchError, TorrentSearchResult};

mod bytes;
mod constants;
mod file_manager;
mod manager;
mod metainfo;
mod peer;
mod torrent;

#[tokio::main]
async fn main() {
  let path = Path::new("torrents/simple.torrent");

  let mut manager = TorrentManager::new();

  let _ = manager.add_torrent_from_file(path);

  let _ = manager.download_torrent(0).await;

  // dbg!(torrent.request_tracker(None).await);

  // dbg!(bytes::encode_bytes(&torrent.metainfo.info_hash));

  // dbg!(bytes::bytes_to_hexadecimal(&[
  //   18, 52, 86, 120, 154, 188, 222, 241, 35, 69, 103, 137, 171, 205, 239, 18, 52, 86, 120, 154
  // ]));

  // let metainfo = metainfo::MetaInfo::from_file(&path).unwrap();
  // dbg!(metainfo.info_hash);

  // let search_results = search_l337x("Linux".to_string()).await.unwrap();

  // for result in search_results {
  //   println!(
  //     "Name of torrent: {}\nMagnet: {}\nSeeders: {}\nLeeches: {}\n\n",
  //     result.name,
  //     result.magnet.unwrap(),
  //     result.seeders.unwrap(),
  //     result.leeches.unwrap()
  //   );
  // }
}
