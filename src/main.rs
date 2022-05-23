#![feature(array_chunks)]

use std::path::Path;
use torrent::Torrent;
// use torrent_search::{search_l337x, TorrentSearchError, TorrentSearchResult};

mod bytes;
mod manager;
mod metainfo;
mod torrent;

#[tokio::main]
async fn main() {
  let path = Path::new("torrents/ubuntu.torrent");

  // let torrent = Torrent::from_file(path, 6969).unwrap();
  // dbg!(bytes::encode_bytes(&torrent.metainfo.info_hash));

  // dbg!(torrent.request_tracker(None).await);
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
