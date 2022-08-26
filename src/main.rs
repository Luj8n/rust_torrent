use std::path::Path;

use crate::manager::TorrentManager;

mod bytes;
mod constants;
mod file_manager;
mod manager;
mod metainfo;
mod peer;
mod torrent;

#[tokio::main]
async fn main() {
  println!("Starting...");

  let path = Path::new("torrents/test.torrent");

  let mut manager = TorrentManager::new();

  manager.add_torrent_from_file(path).unwrap();
  // dbg!(manager.free_port());

  println!("Done!");
}
