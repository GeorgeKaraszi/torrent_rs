use crate::peer::Peers;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct TackerRequest {
    pub peer_id: String,
    pub port: u16,
    pub uploaded: u16,
    pub downloaded: u16,
    pub left: usize,
    pub compact: usize,
}

#[derive(Debug, Deserialize)]
#[warn(dead_code)]
pub struct TrackerResponse {
    #[serde(rename = "interval")]
    pub _interval: u32,
    pub peers: Peers,
}
