use hex::FromHex;
use sha1::{digest::generic_array::GenericArray, digest::typenum::U20};

pub type Result<T> = anyhow::Result<T, anyhow::Error>;

pub type HashType = [u8; 20];
pub type PeerId = HashType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct HashId(pub HashType);

impl std::ops::Deref for HashId {
    type Target = [u8; 20];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for HashId {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// Allow conversion from [u8; 20]
impl From<[u8; 20]> for HashId {
    fn from(arr: [u8; 20]) -> Self {
        Self(arr)
    }
}

// Allow conversion to [u8; 20]
impl From<HashId> for [u8; 20] {
    fn from(hash: HashId) -> Self {
        hash.0
    }
}

// Implement AsMut for hex::decode_to_slice
impl AsMut<[u8]> for HashId {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

// Implement AsRef for consistency
impl AsRef<[u8]> for HashId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl FromHex for HashId {
    type Error = anyhow::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self> {
        let mut out = Self::default();
        hex::decode_to_slice(hex, &mut out.0 as &mut [u8])?;
        Ok(out)
    }
}

impl TryFrom<GenericArray<u8, U20>> for HashId {
    type Error = anyhow::Error;

    fn try_from(arr: GenericArray<u8, U20>) -> Result<Self> {
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&arr);
        Ok(HashId(hash))
    }
}

impl HashId {
    pub fn default() -> Self {
        Self([0; 20])
    }
    pub fn from_hex(hex: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(hex)?;
        if bytes.len() != 20 {
            anyhow::bail!("Invalid hash length");
        }

        let mut hash = [0; 20];
        hash.copy_from_slice(&bytes);
        Ok(Self(hash))
    }
    pub fn url_encode(&self) -> String {
        let mut encoded = String::with_capacity(self.0.len() * 3);
        for byte in self.iter() {
            encoded.push('%');
            encoded.push_str(hex::encode(&[*byte]).as_str());
        }

        encoded
    }
}
