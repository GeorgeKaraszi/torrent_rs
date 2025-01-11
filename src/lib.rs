use serde::de::{Deserializer, Deserialize};
use serde::ser::{Serializer, Serialize};
use sha1::{Sha1, Digest};

#[derive(Debug)]
pub struct PiecesHashes(Vec<[u8;20]>);

impl PiecesHashes {
    pub fn iter(&self) -> impl Iterator<Item = &[u8; 20]> {
        self.0.iter()
    }

    pub fn hash(&self) -> String {
        let mut hasher = Sha1::new();
        hasher.update(serde_bencode::to_bytes(&self).unwrap());
        format!("{:x}", hasher.finalize())
    }
}

impl <'de> Deserialize<'de> for PiecesHashes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let mut hashes = Vec::new();
        for chunk in bytes.chunks(20) {
            let mut hash = [0u8; 20];
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
        let mut bytes = Vec::new();
        for hash in &self.0 {
            bytes.extend_from_slice(hash);
        }

        bytes.serialize(serializer)
    }
}
