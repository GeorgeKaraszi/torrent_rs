use crate::peer::Peers;
use crate::types::{HashId, PeerId};
use anyhow::{Error, Result};
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

#[derive(Debug, Deserialize, Clone)]
#[warn(dead_code)]
pub struct Tracker {
    #[serde(rename = "interval")]
    pub _interval: u32,
    pub peers: Peers,
}

impl Tracker {
    pub async fn discover_peers(
        peer_id: &PeerId,
        tracker_url: &str,
        info_hash: HashId,
        left: usize,
    ) -> Result<Self, Error> {
        let req = TackerRequest {
            peer_id: std::str::from_utf8(peer_id)?.to_string(),
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left: left,
            compact: 1,
        };

        let params = serde_urlencoded::to_string(&req)?;
        let encoded_info_hash = info_hash.url_encode();
        let url = format!("{}?{}&info_hash={}", tracker_url, params, encoded_info_hash);

        let response = reqwest::get(&url).await.expect("request failed");
        let response = response.bytes().await.expect("response should be ok");
        let response = serde_bencode::from_bytes::<Tracker>(&response)?;

        Ok(response)
    }
}
