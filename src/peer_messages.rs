use crate::peer_messages::MessageTag::UNKNOWN;
use crate::torrent::TorrentInfo;
use crate::types::{HashId, PeerId, Result};
use crate::{decode_bencode, PEER_ID};
use anyhow::Error;
use bytes::{Buf, BufMut, BytesMut};
use std::fmt::Debug;
use tokio_util::codec::{Decoder, Encoder};
// ----------------- CONSTANTS & TYPE ALIASES -----------------

const HANDSHAKE_LENGTH: usize = size_of::<PeerHandshake>(); // 68
const MAX_MESSAGE_LENGTH: usize = 1 << 16;

pub type PeerMessageBuffer = PeerMessage<Vec<u8>>;
pub type PeerExtMessageBuffer<T = Vec<u8>> = PeerMessage<ExtensionPayload<T>>;

// ----------------- TRAIT -----------------

pub trait MessageSerialization {
    fn serialize(&self) -> Result<Vec<u8>>;
    fn deserialize(data: &[u8]) -> Result<Self>
    where
        Self: Sized;
}

trait SendableMessage {}

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
    Extension = 20,
    UNKNOWN = 255,
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

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PieceRequestMessage {
    index: [u8; 4],
    begin: [u8; 4],
    length: [u8; 4],
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionHandshakeMessage {
    #[serde(rename = "m")]
    pub metadata: ExtensionHandshakeMetaData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_size: Option<u32>,
    #[serde(skip_serializing, flatten)]
    pub _data: serde_json::Value,
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
    pub torrent_info: Option<TorrentInfo>,
}

// ----------------- IMPLEMENTATIONS -----------------

impl MessageTag {
    pub fn from_u8(tag: u8) -> Result<Self> {
        match tag {
            0 => Ok(Self::Choke),
            1 => Ok(Self::Unchoke),
            2 => Ok(Self::Interested),
            3 => Ok(Self::NotInterested),
            4 => Ok(Self::Have),
            5 => Ok(Self::Bitfield),
            6 => Ok(Self::Request),
            7 => Ok(Self::Piece),
            8 => Ok(Self::Cancel),
            20 => Ok(Self::Extension),
            // _ => anyhow::bail!("Unknown tag {}", tag),
            _ => {
                println!("UNKNOWN TAG: {:?}", tag);
                Ok(UNKNOWN)
            }
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

impl SendableMessage for PeerHandshake {}

impl MessageSerialization for PeerHandshake {
    fn serialize(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).map_err(Into::into)
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        anyhow::ensure!(
            data.len() == HANDSHAKE_LENGTH,
            "Invalid handshake size Expected {} Got {}",
            HANDSHAKE_LENGTH,
            data.len()
        );

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

impl<T> SendableMessage for PeerMessage<T> where T: MessageSerialization {}

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
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&data[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize - 1;

        let message_tag = MessageTag::from_u8(data[4])?;
        let payload_data = &data[5..5 + length];

        anyhow::ensure!(
            payload_data.len() == length,
            "Invalid payload size Expected {} Got {}",
            length,
            payload_data.len()
        );

        let payload = T::deserialize(&data[5..])?;

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
            metadata_size: None,
            _data: serde_json::Value::Null,
        }
    }
}

impl MessageSerialization for ExtensionHandshakeMessage {
    fn serialize(&self) -> Result<Vec<u8>> {
        serde_bencode::to_bytes(&self).map_err(Into::into)
    }

    fn deserialize(data: &[u8]) -> Result<Self> {
        decode_bencode(data)
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

impl ExtensionPieceRequestMessage {
    pub fn default() -> Self {
        Self {
            msg_type: 0,
            piece: 0,
            total_size: 0,
            torrent_info: None,
        }
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
            let torrent_info: TorrentInfo = serde_bencode::from_bytes(piece_data).unwrap();

            payload.torrent_info = Some(torrent_info);
        }

        Ok(payload)
    }
}

// ----------------- CODEC -----------------

#[derive(Debug)]
pub enum PeerFrame {
    Handshake(PeerHandshake),
    Message(PeerMessageBuffer),
}

#[derive(Debug, PartialEq)]
pub enum CodecState {
    WaitingHandshake,
    Messages,
}

pub struct PeerCodec {
    pub state: CodecState,
}

impl PeerCodec {
    pub fn new() -> Self {
        Self {
            state: CodecState::WaitingHandshake,
        }
    }
}

impl PeerFrame {
    pub fn expect_handshake(self) -> Result<PeerHandshake> {
        match self {
            PeerFrame::Handshake(handshake) => Ok(handshake),
            _ => anyhow::bail!("Expected PeerFrame::Handshake got {:#?}", self),
        }
    }

    pub fn expect_message<T>(self) -> Result<PeerMessage<T>>
    where
        T: MessageSerialization + Debug,
    {
        match self {
            PeerFrame::Message(message) => message.convert::<T>(),
            _ => anyhow::bail!("Expected PeerFrame::Message got {:#?}", self),
        }
    }

    pub fn expect_extension_message<T>(self) -> Result<PeerMessage<ExtensionPayload<T>>>
    where
        T: MessageSerialization + Debug,
    {
        self.expect_message::<ExtensionPayload<T>>()
    }

    pub fn unwrap_message(self) -> PeerMessageBuffer {
        match self {
            PeerFrame::Message(message) => message,
            _ => panic!("Expected PeerFrame::Message got {:#?}", self),
        }
    }
}

impl Decoder for PeerCodec {
    type Item = PeerFrame;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        match self.state {
            CodecState::WaitingHandshake => {
                if src.len() < HANDSHAKE_LENGTH {
                    return Ok(None);
                }

                let handshake = PeerHandshake::deserialize(&src[..HANDSHAKE_LENGTH])?;
                src.advance(HANDSHAKE_LENGTH);

                self.state = CodecState::Messages;
                Ok(Some(PeerFrame::Handshake(handshake)))
            }
            CodecState::Messages => {
                if src.len() < 4 {
                    return Ok(None);
                }

                let mut length_bytes = [0u8; 4];
                length_bytes.copy_from_slice(&src[..4]);
                let length = u32::from_be_bytes(length_bytes) as usize;
                let message_length = length + 4;

                if length == 0 {
                    src.advance(4);
                    return Ok(None);
                }

                if src.len() < 5 {
                    return Ok(None);
                }

                if length > MAX_MESSAGE_LENGTH {
                    anyhow::bail!(
                        "Message length exceeds maximum allowed length: Max {} Got {}",
                        MAX_MESSAGE_LENGTH,
                        length
                    );
                }

                if src.len() < message_length {
                    src.reserve(message_length - src.len());
                    return Ok(None);
                }

                let payload = &src[..message_length];
                let peer_message = PeerMessageBuffer::deserialize(payload)?;
                src.advance(message_length);

                Ok(Some(PeerFrame::Message(peer_message)))
            }
        }
    }
}

impl<T> Encoder<T> for PeerCodec
where
    T: MessageSerialization + SendableMessage,
{
    type Error = Error;

    fn encode(&mut self, item: T, dst: &mut BytesMut) -> Result<()> {
        dst.extend(item.serialize()?);
        Ok(())
    }
}
