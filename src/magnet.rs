use crate::peer::PeerConnection;
use crate::tracker::Tracker;
use crate::types::{HashId, PeerId};
use anyhow::{anyhow, Error, Result};
use reqwest::Url;
use std::ops::Deref;

#[derive(Debug, Clone)]
pub struct Magnet {
    pub name: Option<String>,
    pub info_hash: HashId,
    pub announce: Option<String>,
    tracker: Option<Tracker>,
}

impl Magnet {
    pub fn from_str(magnet: &str) -> Result<Self, Error> {
        let mut info_hash = None;
        let mut announce: Option<String> = None;
        let mut name: Option<String> = None;

        let url = Url::parse(magnet)?;

        for (query_name, value) in url.query_pairs() {
            match query_name.deref() {
                "xt" => match value.deref().strip_prefix("urn:btih:") {
                    Some(info_hash_hex) => {
                        info_hash = Some(HashId::from_hex(info_hash_hex)?);
                    }
                    None => {
                        return Err(anyhow!("invalid info hash"));
                    }
                },
                "dn" => {
                    name = Some(value.into_owned());
                }
                "tr" => {
                    announce = Some(value.into_owned());
                }
                _ => {}
            }
        }

        Ok(Self {
            name: name,
            info_hash: info_hash.expect("no hash found"),
            announce: announce,
            tracker: None,
        })
    }

    pub fn announce(&self) -> &str {
        self.announce
            .as_ref()
            .expect("invalid tracker url")
            .as_str()
    }

    pub fn tracker(&self) -> &Tracker {
        match self.tracker {
            Some(ref tracker) => tracker,
            None => panic!("tracker not found, you must call discover_peers first"),
        }
    }

    pub async fn discover_peers(&mut self, peer_id: &PeerId) -> Result<&Tracker, Error> {
        if self.tracker.is_none() {
            self.tracker = Tracker::discover_peers(peer_id, self.announce(), self.info_hash, 20)
                .await
                .ok();
        }

        Ok(self.tracker())
    }

    pub async fn handshake(&mut self, peer_id: &PeerId) -> Result<PeerConnection, Error> {
        let tracker = self.discover_peers(peer_id).await?;

        let mut peer = PeerConnection::connect(tracker.peers[0].clone()).await?;
        peer.send_handshake(self.info_hash, true).await?;
        peer.send_extension_handshake().await?;

        println!("Peer ID: {}", hex::encode(peer.peer_id()));
        println!("Peer Metadata Extension ID: {}", peer.metadata_id());

        Ok(peer)
    }

    pub async fn request_info(&mut self, peer_id: &PeerId) -> Result<(), Error> {
        let _peer = self.handshake(peer_id).await?;

        Ok(())
    }
}
