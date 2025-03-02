use crate::torrent::TorrentInfo;
use crate::types::{HashId, PeerId, Result};
use crate::{decode_bencode, PEER_ID};
use bytes::{Buf, BufMut, BytesMut};
use std::fmt::Debug;
use std::io::{Cursor, Read};
// ----------------- CONSTANTS & TYPE ALIASES -----------------

pub type PeerMessageBuffer = PeerMessage<Vec<u8>>;
pub type PeerExtMessageBuffer<T = Vec<u8>> = PeerMessage<ExtensionPayload<T>>;

// ----------------- TRAIT -----------------

pub trait MessageSerialization {
    fn serialize(&self) -> Result<Vec<u8>>;
    fn deserialize(data: &[u8]) -> Result<Self>
    where
        Self: Sized;
}

// ----------------- MESSAGE TAGS -----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[repr(u8)]
pub enum MessageTag {
    Choke = 0,
    Unchoke = 1,
    Interested = 2,
    NotInterested = 3,
    Have = 4,
    Bitfield = 5,
    Request = 6,
    Piece = 7,
    Cancel = 8,
    RejectRequest = 16,
    Extension = 20,
}

// ----------------- MESSAGE STRUCTS -----------------

#[repr(C)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PeerHandshake {
    pub protocol_length: u8,
    pub protocol: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: HashId,
    pub peer_id: PeerId,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PeerMessage<T>
where
    T: MessageSerialization,
{
    pub message_tag: MessageTag,
    pub message: T,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionPayload<T>
where
    T: MessageSerialization,
{
    pub metadata_id: u8,
    pub payload: T,
}

#[repr(C)]
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PieceRequestMessage {
    index: [u8; 4],
    begin: [u8; 4],
    length: [u8; 4],
}

#[repr(C)]
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct PieceMessage {
    index: [u8; 4],
    begin: [u8; 4],
    block: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionHandshakeMessage {
    #[serde(rename = "m")]
    pub metadata: ExtensionHandshakeMetaData,
    #[serde(rename = "p")]
    pub local_listen_port: Option<u16>,

    pub metadata_size: Option<u32>,
    #[serde(rename = "reqq")]
    pub max_outstanding_requests: Option<u32>,
    #[serde(rename = "v")]
    pub version: Option<String>,
    #[serde(skip, rename = "yourip")]
    pub your_ip: Option<std::net::IpAddr>,
    #[serde(skip)]
    pub ipv6: Option<std::net::Ipv6Addr>,
    #[serde(skip)]
    pub ipv4: Option<std::net::Ipv4Addr>,
    // #[serde(skip_serializing, flatten)]
    // pub _data: serde_json::Value,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExtensionHandshakeMetaData {
    pub ut_metadata: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ut_pex: Option<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionPieceRequestMessage {
    pub msg_type: u8,
    pub piece: u32,
    pub total_size: u32,
    #[serde(skip)]
    torrent_info: Option<TorrentInfo>,
}

// ----------------- IMPLEMENTATIONS -----------------

impl MessageTag {
    pub fn from_u8(tag: u8) -> Result<Self> {
        match tag {
            x if x == Self::Choke as u8 => Ok(Self::Choke),
            x if x == Self::Unchoke as u8 => Ok(Self::Unchoke),
            x if x == Self::Interested as u8 => Ok(Self::Interested),
            x if x == Self::NotInterested as u8 => Ok(Self::NotInterested),
            x if x == Self::Have as u8 => Ok(Self::Have),
            x if x == Self::Bitfield as u8 => Ok(Self::Bitfield),
            x if x == Self::Request as u8 => Ok(Self::Request),
            x if x == Self::Piece as u8 => Ok(Self::Piece),
            x if x == Self::Cancel as u8 => Ok(Self::Cancel),
            x if x == Self::RejectRequest as u8 => Ok(Self::RejectRequest),
            x if x == Self::Extension as u8 => Ok(Self::Extension),
            _ => anyhow::bail!("Unknown tag {}", tag),
        }
    }
}

impl PeerHandshake {
    pub fn default(info_hash: Option<HashId>, peer_id: Option<PeerId>) -> Self {
        Self {
            protocol_length: 19,
            protocol: *b"BitTorrent protocol",
            reserved: [0; 8],
            info_hash: info_hash.unwrap_or(HashId::default()),
            peer_id: peer_id.unwrap_or(*PEER_ID),
        }
    }

    pub fn new(info_hash: Option<HashId>, peer_id: Option<PeerId>, extension: bool) -> Self {
        let mut handshake = Self::default(info_hash, peer_id);
        if extension {
            handshake.reserved = (1u64 << 20).to_be_bytes().into();
        }

        handshake
    }
}

impl MessageSerialization for PeerHandshake {
    fn serialize(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).map_err(Into::into)
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        bincode::deserialize(data).map_err(Into::into)
    }
}

impl<T> PeerMessage<T>
where
    T: MessageSerialization,
{
    pub fn new(message_tag: MessageTag, payload: T) -> Self {
        Self {
            message_tag,
            message: payload,
        }
    }
}

impl PeerMessage<Vec<u8>> {
    pub fn convert<D>(&self) -> Result<PeerMessage<D>>
    where
        D: MessageSerialization,
    {
        Ok(PeerMessage::new(
            self.message_tag,
            D::deserialize(&self.message)?,
        ))
    }
}

impl<T> MessageSerialization for PeerMessage<T>
where
    T: MessageSerialization,
{
    fn serialize(&self) -> Result<Vec<u8>> {
        let encoded_payload = self.message.serialize()?;
        let payload_size: [u8; 4] = u32::to_be_bytes(encoded_payload.len() as u32 + 1);
        let message_tag = self.message_tag as u8;

        let mut buffer = BytesMut::new();
        buffer.extend_from_slice(&payload_size);
        buffer.put_u8(message_tag);
        buffer.extend_from_slice(&encoded_payload);
        Ok(buffer.to_vec())
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(data);
        let length = cursor.get_u32() as usize - 1;

        let message_tag = MessageTag::from_u8(cursor.get_u8())?;

        let mut payload_data = vec![0; length];
        cursor.read_exact(&mut payload_data)?;

        anyhow::ensure!(
            payload_data.len() == length,
            "Invalid payload size Expected {} Got {}",
            length,
            payload_data.len()
        );

        let payload = T::deserialize(&payload_data)?;

        Ok(Self::new(message_tag, payload))
    }
}

impl<T> MessageSerialization for ExtensionPayload<T>
where
    T: MessageSerialization,
{
    fn serialize(&self) -> Result<Vec<u8>> {
        let mut bytes = BytesMut::new();
        bytes.put_u8(self.metadata_id);
        bytes.extend(self.payload.serialize()?);
        Ok(bytes.to_vec())
    }

    fn deserialize(data: &[u8]) -> Result<ExtensionPayload<T>> {
        Ok(ExtensionPayload {
            metadata_id: data[0],
            payload: T::deserialize(&data[1..])?,
        })
    }
}

impl MessageSerialization for Vec<u8> {
    fn serialize(&self) -> Result<Vec<u8>> {
        Ok(self.clone())
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        Ok(data.to_vec())
    }
}

impl<T> ExtensionPayload<T>
where
    T: MessageSerialization,
{
    pub fn new(metadata_id: u8, payload: T) -> Self {
        Self {
            metadata_id,
            payload,
        }
    }

    pub fn into_peer_message(self) -> PeerMessage<Self> {
        PeerMessage::new(MessageTag::Extension, self)
    }
}

impl ExtensionHandshakeMessage {
    pub fn default() -> Self {
        Self {
            metadata: ExtensionHandshakeMetaData {
                ut_metadata: 1,
                ut_pex: None,
            },
            local_listen_port: None,
            metadata_size: None,
            max_outstanding_requests: None,
            version: None,
            your_ip: None,
            ipv6: None,
            ipv4: None,
        }
    }
}

impl MessageSerialization for ExtensionHandshakeMessage {
    fn serialize(&self) -> Result<Vec<u8>> {
        serde_bencode::to_bytes(&self).map_err(Into::into)
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        serde_bencode::from_bytes(data).map_err(Into::into)
    }
}

impl PieceRequestMessage {
    pub fn new(index: u32, begin: u32, length: u32) -> Self {
        Self {
            index: index.to_be_bytes(),
            begin: begin.to_be_bytes(),
            length: length.to_be_bytes(),
        }
    }

    pub fn index(&self) -> u32 {
        u32::from_be_bytes(self.index)
    }

    pub fn begin(&self) -> u32 {
        u32::from_be_bytes(self.begin)
    }

    pub fn length(&self) -> u32 {
        u32::from_be_bytes(self.length)
    }
}

impl MessageSerialization for PieceRequestMessage {
    fn serialize(&self) -> Result<Vec<u8>> {
        bincode::serialize(&self).map_err(Into::into)
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        bincode::deserialize(data).map_err(Into::into)
    }
}

impl PieceMessage {
    pub fn new(index: u32, begin: u32, block: Vec<u8>) -> Self {
        Self {
            index: index.to_be_bytes(),
            begin: begin.to_be_bytes(),
            block,
        }
    }

    pub fn index(&self) -> u32 {
        u32::from_be_bytes(self.index)
    }

    pub fn begin(&self) -> u32 {
        u32::from_be_bytes(self.begin)
    }

    pub fn block(&self) -> &[u8] {
        &self.block
    }

    pub fn length(&self) -> u32 {
        self.block.len() as u32
    }
}

impl MessageSerialization for PieceMessage {
    fn serialize(&self) -> Result<Vec<u8>> {
        bincode::serialize(&self).map_err(Into::into)
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(data);
        let index = cursor.get_u32();
        let begin = cursor.get_u32();
        let mut block = Vec::new();
        cursor.read_to_end(&mut block)?;

        Ok(Self::new(index, begin, block))
    }
}

impl ExtensionPieceRequestMessage {
    pub fn default() -> Self {
        Self {
            msg_type: 0,
            piece: 0,
            total_size: 0,
            torrent_info: None,
        }
    }

    pub fn torrent_info(&self) -> &TorrentInfo {
        self.torrent_info.as_ref().expect("torrent info missing")
    }
}

impl MessageSerialization for ExtensionPieceRequestMessage {
    fn serialize(&self) -> Result<Vec<u8>> {
        let torrent_info_serialized = if let Some(info) = &self.torrent_info {
            serde_bencode::to_bytes(info)?
        } else {
            vec![]
        };

        let mut owned_self = self.clone();
        owned_self.total_size = torrent_info_serialized.len() as u32;

        let mut buffer = BytesMut::new();
        buffer.extend(serde_bencode::to_bytes(&owned_self)?);
        buffer.extend(torrent_info_serialized);
        Ok(buffer.to_vec())
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        let mut payload: Self = decode_bencode(data)?;

        if payload.total_size > 0 {
            let piece_data = &data[(data.len() - (payload.total_size as usize))..];
            let torrent_info: TorrentInfo = serde_bencode::from_bytes(piece_data)?;

            payload.torrent_info = Some(torrent_info);
        }

        Ok(payload)
    }
}
