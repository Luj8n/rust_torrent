use crate::{
  constants::{BLOCK_SIZE, ROLLING_AVERAGE_SIZE},
  metainfo::MetaInfo,
  peer::BlockInfo,
};

pub struct PeerInfo {
  peer_id: Option<[u8; 20]>,

  pub am_choking: bool,
  pub am_interested: bool,

  pub peer_choking: bool,
  pub peer_interested: bool,

  total_downloaded_from: u32,
  total_uploaded_to: u32,

  prev_total_downloaded_from: u32,
  prev_total_uploaded_to: u32,

  rolling_download: Vec<u32>,
  rolling_upload: Vec<u32>,

  queue_size: usize,
  pub piece_availability: Vec<bool>,
  pub requested_blocks: Vec<BlockInfo>,

  in_slow_start: bool,
}

impl PeerInfo {
  pub fn new(metainfo: &MetaInfo) -> Self {
    PeerInfo {
      peer_id: None,
      am_choking: true,
      am_interested: false,
      peer_choking: true,
      peer_interested: false,
      total_downloaded_from: 0,
      total_uploaded_to: 0,
      prev_total_downloaded_from: 0,
      prev_total_uploaded_to: 0,
      rolling_download: vec![0],
      rolling_upload: vec![0],
      queue_size: 2,
      piece_availability: vec![false; metainfo.pieces.len()],
      requested_blocks: vec![],
      in_slow_start: true,
    }
  }
  // should run every second
  pub fn tick(&mut self) {
    self.update_speed_averages();
    self.update_queue_size();
    self.try_exit_slow_start();

    // println!(
    //   "Queue size = {}, Download rate = {}Mb/s",
    //   self.queue_size,
    //   self.download_rate() / 1024 / 1024
    // );
  }

  pub fn add_peer_id(&mut self, peer_id: [u8; 20]) {
    assert!(self.peer_id.is_none());

    self.peer_id = Some(peer_id);
  }

  pub fn try_remove_job(
    &mut self,
    block_info: BlockInfo,
    available_jobs: &mut Vec<BlockInfo>,
  ) -> bool {
    // returns true if the job was removed

    if let Some(pos) = self.requested_blocks.iter().position(|x| *x == block_info) {
      self.requested_blocks.remove(pos);

      available_jobs.push(block_info);

      return true;
    }

    false
  }

  pub fn try_get_new_jobs(
    &mut self,
    available_jobs: &mut Vec<BlockInfo>,
  ) -> Option<Vec<BlockInfo>> {
    if self.requested_blocks.len() >= self.queue_size
      || self.peer_choking
      || available_jobs.is_empty()
    {
      return None;
    }

    let mut just_requested = vec![];

    while self.requested_blocks.len() < self.queue_size && !available_jobs.is_empty() {
      for i in 0..available_jobs.len() {
        if self.piece_availability[available_jobs[i].index as usize] {
          let block_info = available_jobs.remove(i);
          self.requested_blocks.push(block_info);
          just_requested.push(block_info);

          break;
        }
      }
    }

    Some(just_requested)
  }

  pub fn check_if_interested(&self, downloaded_pieces: &[bool]) -> bool {
    assert_eq!(downloaded_pieces.len(), self.piece_availability.len());

    downloaded_pieces
      .iter()
      .zip(self.piece_availability.iter())
      .any(|(our, peers)| *our && *peers)
  }

  pub fn downloaded_chunk(&mut self, length: u32) {
    self.total_downloaded_from += length;

    self.queue_size += 1;
  }

  pub fn uploaded_chunk(&mut self, length: u32) {
    self.total_uploaded_to += length;
  }

  fn update_queue_size(&mut self) {
    if !self.in_slow_start {
      self.queue_size = (self.download_rate() / BLOCK_SIZE).max(1) as usize;
    }
  }

  fn try_exit_slow_start(&mut self) {
    if self.in_slow_start {
      assert!(!self.rolling_download.is_empty());

      if self.rolling_download[0] + 10000 < self.download_rate() {
        self.in_slow_start = false;
      }
    }
  }

  fn update_speed_averages(&mut self) {
    assert!(!self.rolling_download.is_empty());
    assert!(!self.rolling_upload.is_empty());

    let download_delta = self.total_downloaded_from - self.prev_total_downloaded_from;
    let upload_delta = self.total_uploaded_to - self.prev_total_uploaded_to;

    self.rolling_download.insert(0, download_delta);
    self.rolling_upload.insert(0, upload_delta);

    self.rolling_download.truncate(ROLLING_AVERAGE_SIZE);
    self.rolling_upload.truncate(ROLLING_AVERAGE_SIZE);

    self.prev_total_downloaded_from = self.total_downloaded_from;
    self.prev_total_uploaded_to = self.total_uploaded_to;
  }

  pub fn download_rate(&self) -> u32 {
    self.rolling_download.iter().sum::<u32>() / self.rolling_download.len() as u32
  }

  pub fn upload_rate(&self) -> u32 {
    self.rolling_upload.iter().sum::<u32>() / self.rolling_upload.len() as u32
  }
}
