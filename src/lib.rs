use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};

#[derive(Debug)]
pub struct PiecesHashes(Vec<[u8; 20]>);

impl PiecesHashes {
    pub fn iter(&self) -> impl Iterator<Item = &[u8; 20]> {
        self.0.iter()
    }

    pub fn hashes(&self) -> Vec<String> {
        let mut hashes = Vec::new();
        for hash in &self.0 {
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
        let flat: Vec<u8> = self.0.iter().flat_map(|x| x.iter().copied()).collect();
        serde_bytes::Serialize::serialize(&flat, serializer)
    }
}
