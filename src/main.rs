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

  let path = Path::new("torrents/some.torrent"); // TODO: create some torrent and seed it myself

  let mut manager = TorrentManager::new();

  manager.add_torrent_from_file(path).unwrap();
  manager.torrents[0].start_downloading(); // for checking never used functions/variables

  println!("Done!");
}
