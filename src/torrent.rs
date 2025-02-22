use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use sha1::{Digest, Sha1};
use std::ops::{Deref, DerefMut};
use crate::types::HashId;

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Torrent {
    pub announce: String,
    pub info: TorrentInfo,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TorrentInfo {
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    pub pieces: PiecesHashes,
    pub length: usize,
}

#[derive(Debug, Clone)]
pub struct PiecesHashes(Vec<HashId>);

impl TorrentInfo {
    pub fn hash(&self) -> HashId {
        let mut hasher = Sha1::new();
        Digest::update(&mut hasher, serde_bencode::to_bytes(&self).unwrap());
        hasher.finalize().try_into().unwrap()
    }

    pub fn encoded_hash(&self) -> String {
        let hashed = self.hash();
        let mut encoded = String::with_capacity(hashed.len() * 3);
        for byte in hashed.iter() {
            encoded.push('%');
            encoded.push_str(hex::encode(&[*byte]).as_str());
        }

        encoded
    }
}

impl Deref for PiecesHashes {
    type Target = Vec<HashId>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PiecesHashes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl PiecesHashes {
    pub fn iter(&self) -> impl Iterator<Item = &HashId> {
        self.0.iter()
    }

    pub fn encoded_hashes(&self) -> Vec<String> {
        let mut hashes = Vec::new();
        for hash in self.iter() {
            hashes.push(hex::encode(hash));
        }

        hashes
    }
}

impl<'de> Deserialize<'de> for PiecesHashes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: Vec<u8> = serde_bytes::Deserialize::deserialize(deserializer)?;

        if bytes.len() % 20 != 0 {
            return Err(serde::de::Error::custom(
                "Invalid Piece length. Must be multiple of 20.",
            ));
        }

        let mut hashes = Vec::new();
        for chunk in bytes.chunks(20) {
            let mut hash = HashId::default();
            hash.copy_from_slice(chunk);
            hashes.push(hash);
        }

        Ok(PiecesHashes(hashes))
    }
}

impl Serialize for PiecesHashes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let flat: Vec<u8> = self.0.iter().flat_map(|x| x.iter().copied()).collect();
        serde_bytes::Serialize::serialize(&flat, serializer)
    }
}
