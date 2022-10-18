use std::path::Path;

use crate::manager::TorrentManager;

mod bytes;
mod constants;
mod file_manager;
mod manager;
mod metainfo;
mod peer;
mod peer_info;
mod torrent;

#[tokio::main]
async fn main() {
  // FIXME: find the deadlock. trying to trace but it doesn't work for some reason
  console_subscriber::init();

  println!("Starting...");

  let path = Path::new("torrents/testing.torrent");

  let mut manager = TorrentManager::new();

  manager.add_torrent_from_file(path).unwrap();

  // let p = manager.torrents[0].check_whole_hash().await.iter().all(|x| *x);
  // dbg!(p);
  manager.torrents[0].start_downloading();

  loop {}
}
