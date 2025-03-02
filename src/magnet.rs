use crate::peer::PeerConnection;
use crate::torrent::TorrentInfo;
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
    torrent_info: Option<TorrentInfo>,
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
            torrent_info: None,
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

    pub fn torrent_info(&self) -> &TorrentInfo {
        self.torrent_info.as_ref().expect("torrent info missing")
    }

    pub async fn discover_peers(&mut self, peer_id: &PeerId) -> Result<&Tracker, Error> {
        if self.tracker.is_none() {
            self.tracker =
                Some(Tracker::discover_peers(peer_id, self.announce(), self.info_hash, 20).await?);
        }

        Ok(self.tracker())
    }

    pub async fn handshake(&self, peer: &mut PeerConnection) -> Result<()> {
        if peer.requires_handshake() {
            peer.send_handshake(self.info_hash.clone(), true).await?;
        }

        if peer.requires_extension_exchange() {
            peer.exchange_magnet_info().await?;
        }

        Ok(())
    }

    pub async fn retrieve_magnet_info(&mut self, peer: &mut PeerConnection) -> Result<()> {
        self.handshake(peer).await?;

        let request = peer.request_magnet_torrent_info().await?;
        self.torrent_info = Some(request.message.payload.torrent_info().clone());

        Ok(())
    }
}
