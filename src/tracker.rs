use serde::{Deserialize, Deserializer, Serialize};
use std::net::{IpAddr, Ipv4Addr};

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
    pub interval: u32,
    pub peers: Peers,
}

#[derive(Debug)]
pub struct Peers(Vec<Peer>);

#[derive(Debug)]
pub struct Peer {
    pub ip: IpAddr,
    pub port: u16,
}

impl Peers {
    pub fn iter(&self) -> impl Iterator<Item = &Peer> {
        self.0.iter()
    }
}

impl<'de> Deserialize<'de> for Peers {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: Vec<u8> = serde_bytes::deserialize(deserializer)?;

        if bytes.len() % 6 != 0 {
            return Err(serde::de::Error::custom(
                "invalid Peer length. Must be multiple of 6.",
            ));
        }

        let mut peers = Vec::new();
        for chunk in bytes.chunks(6) {
            let ip: [u8; 4] = chunk.get(0..4).unwrap().try_into().unwrap();
            let port: [u8; 2] = chunk.get(4..6).unwrap().try_into().unwrap();

            let ip = IpAddr::V4(Ipv4Addr::from(ip));
            let port = u16::from_be_bytes(port);

            peers.push(Peer { ip, port });
        }
        Ok(Self(peers))
    }
}
