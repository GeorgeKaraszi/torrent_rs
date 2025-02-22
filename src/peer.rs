// ----------------- IMPORTS -----------------
use crate::types::{HashId, PeerId};
use crate::{decode_bencode, PEER_ID};
use anyhow::{anyhow, ensure, Context, Error};
use bytes::{Buf, BufMut, BytesMut};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Deserializer};
use std::fmt;
use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio_util::codec::{Decoder, Encoder, Framed};

// ----------------- CONSTANTS & TYPE ALIASES -----------------
const HANDSHAKE_LENGTH: usize = size_of::<PeerHandshake>(); // 68
const MAX_MESSAGE_LENGTH: usize = 1 << 16;

// ----------------- CORE PEER STRUCTS -----------------
#[derive(Debug, Clone)]
pub struct Peers(Vec<Peer>);

#[derive(Debug, Clone)]
pub struct Peer {
    pub ip: IpAddr,
    pub port: u16,
}

#[derive(Clone)]
pub struct PeerConnection {
    pub peer: Peer,
    pub peer_id: Option<PeerId>,
    pub metadata_id: Option<u8>,
    pub connection: Arc<Mutex<Framed<TcpStream, PeerCodec>>>,
}

// ----------------- NETWORKING & PROTOCOL -----------------
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PeerHandshake {
    pub protocol_length: u8,
    pub protocol: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: HashId,
    pub peer_id: PeerId,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerMessageExt {
    pub m: PeerExtDirectory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_size: Option<u32>,
    #[serde(skip_serializing)]
    #[serde(flatten)]
    pub _data: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerExtDirectory {
    pub ut_metadata: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ut_pex: Option<u8>,
}

#[repr(C)]
#[derive(Debug)]
pub struct PeerMessage {
    pub message_tag: MessageTag,
    pub extension: Option<u8>,
    pub payload: Vec<u8>,
}

#[repr(C)]
#[derive(Debug)]
pub struct RequestMessage {
    index: [u8; 4],
    begin: [u8; 4],
    length: [u8; 4],
}

#[repr(C)]
#[derive(Debug)]
pub struct Piece<T: ?Sized = [u8]> {
    index: [u8; 4],
    begin: [u8; 4],
    block: T,
}

// ----------------- ENCODING & DECODING -----------------
pub struct PeerCodec {
    pub state: CodecState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    UNKNOWN = 9,
}

#[derive(Debug)]
pub enum PeerFrame {
    Handshake(PeerHandshake),
    HandshakeExt(PeerMessage),
    Message(PeerMessage),
}

#[derive(Debug, PartialEq)]
pub enum CodecState {
    WaitingHandshake,
    Messages,
}

// Implementations

impl Deref for Peers {
    type Target = Vec<Peer>;

    fn deref(&self) -> &Self::Target {
        &self.0
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

impl Peer {
    pub fn ip_address(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}

impl fmt::Debug for PeerConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerConnection")
            .field("peer", &self.peer)
            .field("connection", &"Framed<TcpStream, PeerCodec>")
            .finish()
    }
}

impl PeerConnection {
    pub async fn connect(peer: Peer) -> anyhow::Result<Self, Error> {
        let connection = TcpStream::connect(peer.ip_address())
            .await
            .context("Failed to connect to peer")?;

        Ok(Self {
            peer: peer,
            peer_id: None,
            metadata_id: None,
            connection: Arc::new(Mutex::new(Framed::new(connection, PeerCodec::new()))),
        })
    }

    pub fn peer_id(&self) -> PeerId {
        match self.peer_id {
            Some(peer_id) => peer_id,
            None => panic!("Peer ID is not set you must send a handshake first"),
        }
    }

    pub fn metadata_id(&self) -> u8 {
        match self.metadata_id {
            Some(metadata_id) => metadata_id,
            None => panic!("Metadata ID is not set you must send an extension handshake first"),
        }
    }

    pub async fn send_handshake(
        &mut self,
        info_hash: HashId,
        extension: bool,
    ) -> anyhow::Result<PeerHandshake, Error> {
        let mut connection = self.connection.lock().expect("connection is locked");

        let handshake = if extension {
            PeerHandshake::new_ext(info_hash)
        } else {
            PeerHandshake::new(info_hash)
        };

        connection.send(PeerFrame::Handshake(handshake)).await?;

        let peer_handshake = connection
            .next()
            .await
            .expect("peer always sends handshake")
            .context("peer handshake was invalid")?
            .expect_handshake();

        ensure!(peer_handshake.protocol_length == 19);
        ensure!(&peer_handshake.protocol == b"BitTorrent protocol");
        ensure!(&peer_handshake.peer_id != PEER_ID);

        let bitfield = connection
            .next()
            .await
            .expect("peer always sends bitfields")
            .context("peer message was invalid")?
            .expect_message();

        ensure!(bitfield.message_tag == MessageTag::Bitfield);

        self.peer_id = Some(peer_handshake.peer_id);

        Ok(peer_handshake)
    }

    pub async fn send_extension(&mut self) -> anyhow::Result<PeerMessageExt, Error> {
        let mut connection = self.connection.lock().expect("connection is locked");

        let ext_msg = PeerMessageExt::new();
        let peer_msg = PeerMessage::extension(ext_msg.encode());
        connection.send(PeerFrame::HandshakeExt(peer_msg)).await?;

        let peer_ext = connection
            .next()
            .await
            .expect("peer always sends extension")
            .context("peer message was invalid")?
            .expect_message();

        ensure!(peer_ext.message_tag == MessageTag::Extension);

        let parsed_ext = PeerMessageExt::from_bytes(peer_ext.payload.as_slice())?;

        self.metadata_id = Some(parsed_ext.m.ut_metadata);

        Ok(parsed_ext)
    }

    pub async fn send_interested(&mut self) -> anyhow::Result<(), Error> {
        let mut connection = self.connection.lock().expect("connection is locked");

        connection
            .send(PeerFrame::Message(PeerMessage::interested()))
            .await?;

        let unchoked = connection
            .next()
            .await
            .expect("peer responses with unchoked")
            .context("peer message was invalid")?
            .expect_message();

        ensure!(unchoked.message_tag == MessageTag::Unchoke);
        ensure!(unchoked.payload.is_empty());
        Ok(())
    }
}

impl PeerMessage {
    pub fn new(message_tag: MessageTag, payload: Vec<u8>, extension: Option<u8>) -> Self {
        Self {
            message_tag: message_tag,
            payload: payload,
            extension: extension,
        }
    }

    pub fn new_buffer(capacity: Option<usize>) -> Self {
        let capacity = capacity.unwrap_or(1024);
        Self::new(MessageTag::UNKNOWN, Vec::with_capacity(capacity), None)
    }

    pub fn interested() -> Self {
        Self::new(MessageTag::Interested, vec![], None)
    }

    pub fn request(index: u32, begin: u32, length: u32) -> Self {
        let mut request = RequestMessage::new(index, begin, length);
        Self::new(MessageTag::Request, Vec::from(request.mut_ptr()), None)
    }

    pub fn extension(payload: Vec<u8>) -> Self {
        Self::new(MessageTag::Extension, payload, Some(0))
    }

    pub fn mut_ptr(&mut self) -> &mut [u8; size_of::<Self>()] {
        let bytes = self as *mut Self as *mut [u8; size_of::<Self>()];
        let bytes: &mut [u8; size_of::<Self>()] = unsafe { &mut *bytes };

        bytes
    }
}

impl PeerCodec {
    pub fn new() -> Self {
        Self {
            state: CodecState::WaitingHandshake,
        }
    }
}

impl Decoder for PeerCodec {
    type Item = PeerFrame;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.state {
            CodecState::WaitingHandshake => {
                if src.len() < HANDSHAKE_LENGTH {
                    return Ok(None);
                }

                let mut peer_handshake = PeerHandshake::new_peer();
                peer_handshake
                    .mut_ptr()
                    .copy_from_slice(&src[..HANDSHAKE_LENGTH]);
                src.advance(HANDSHAKE_LENGTH);

                self.state = CodecState::Messages;
                Ok(Some(PeerFrame::Handshake(peer_handshake)))
            }
            CodecState::Messages => {
                if src.len() < 4 {
                    return Ok(None);
                }

                let mut length_bytes = [0u8; 4];
                length_bytes.copy_from_slice(&src[..4]);
                let length = u32::from_be_bytes(length_bytes) as usize;

                if length == 0 {
                    src.advance(4);
                    return Ok(None);
                }

                if src.len() < 5 {
                    return Ok(None);
                }

                if length > MAX_MESSAGE_LENGTH {
                    return Err(anyhow!("Frame of length {} is too large", length));
                }

                if src.len() < 4 + length {
                    src.reserve(4 + length - src.len());
                    return Ok(None);
                }

                let message_tag = match src[4] {
                    0 => MessageTag::Choke,
                    1 => MessageTag::Unchoke,
                    2 => MessageTag::Interested,
                    3 => MessageTag::NotInterested,
                    4 => MessageTag::Have,
                    5 => MessageTag::Bitfield,
                    6 => MessageTag::Request,
                    7 => MessageTag::Piece,
                    8 => MessageTag::Cancel,
                    20 => MessageTag::Extension,
                    tag => {
                        return Err(anyhow!("Unknown message tag id: {}", tag));
                    }
                };

                let message_payload = if src.len() > 5 {
                    src[5..4 + length].to_vec()
                } else {
                    Vec::new()
                };

                src.advance(4 + length);

                Ok(Some(PeerFrame::Message(PeerMessage::new(
                    message_tag,
                    message_payload,
                    None,
                ))))
            }
        }
    }
}

impl Encoder<PeerFrame> for PeerCodec {
    type Error = Error;

    fn encode(&mut self, item: PeerFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            PeerFrame::Handshake(handshake) => {
                let bytes = unsafe {
                    let ptr = &handshake as *const PeerHandshake as *const u8;
                    std::slice::from_raw_parts(ptr, HANDSHAKE_LENGTH)
                };
                dst.reserve(HANDSHAKE_LENGTH);
                dst.put_slice(bytes);
                Ok(())
            }
            PeerFrame::HandshakeExt(msg) => {
                let message_length: [u8; 4] = u32::to_be_bytes(msg.payload.len() as u32 + 2);
                dst.put_slice(&message_length);
                dst.put_u8(msg.message_tag as u8);
                dst.put_u8(0 as u8);
                dst.put_slice(&msg.payload);
                Ok(())
            }
            PeerFrame::Message(msg) => {
                let message_length: [u8; 4] = u32::to_be_bytes(msg.payload.len() as u32 + 1);
                dst.put_slice(&message_length);
                dst.put_u8(msg.message_tag as u8);
                dst.put_slice(&msg.payload);
                Ok(())
            }
        }
    }
}

impl PeerFrame {
    pub fn expect_handshake(self) -> PeerHandshake {
        match self {
            PeerFrame::Handshake(handshake) => handshake,
            _ => panic!("Called `unwrap_handshake()` on a `Message` variant"),
        }
    }

    pub fn expect_message(self) -> PeerMessage {
        match self {
            PeerFrame::Message(message) => message,
            _ => panic!("Called `unwrap_message()` on a `Handshake` variant"),
        }
    }

    pub fn as_message(&self) -> Option<&PeerMessage> {
        match self {
            PeerFrame::Message(message) => Some(message),
            _ => None,
        }
    }
}

impl PeerHandshake {
    pub fn default(
        info_hash: Option<HashId>,
        peer_id: Option<PeerId>,
        extension: Option<[u8; 8]>,
    ) -> Self {
        let peer_id = peer_id.unwrap_or(*PEER_ID);
        let info_hash = info_hash.unwrap_or(HashId::default());
        let reserved = extension.unwrap_or([0u8; 8]);

        Self {
            protocol_length: 19,
            protocol: *b"BitTorrent protocol",
            reserved: reserved,
            info_hash: info_hash,
            peer_id: peer_id,
        }
    }

    pub fn new(info_hash: HashId) -> Self {
        Self::default(Some(info_hash), None, None)
    }

    pub fn new_ext(info_hash: HashId) -> Self {
        let magnet_ext = (1u64 << 20).to_be_bytes().into();
        Self::default(Some(info_hash), None, Some(magnet_ext))
    }

    pub fn new_peer() -> Self {
        Self::default(None, None, None)
    }

    pub fn mut_ptr(&mut self) -> &mut [u8; size_of::<Self>()] {
        let bytes = self as *mut Self as *mut [u8; size_of::<Self>()];
        let bytes: &mut [u8; size_of::<Self>()] = unsafe { &mut *bytes };

        bytes
    }
}

impl PeerMessageExt {
    pub fn new() -> Self {
        Self {
            m: PeerExtDirectory {
                ut_metadata: 1,
                ut_pex: None,
            },
            metadata_size: None,
            _data: serde_json::Value::Null,
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self, Error> {
        let bytes = bytes[1..].as_ref();
        decode_bencode(bytes)
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_bencode::to_bytes(self).unwrap()
    }
}

impl RequestMessage {
    pub fn new(index: u32, begin: u32, length: u32) -> Self {
        Self {
            index: index.to_be_bytes(),
            begin: begin.to_be_bytes(),
            length: length.to_be_bytes(),
        }
    }
    pub fn mut_ptr(&mut self) -> &mut [u8; size_of::<Self>()] {
        let bytes = self as *mut Self as *mut [u8; size_of::<Self>()];
        let bytes: &mut [u8; size_of::<Self>()] = unsafe { &mut *bytes };

        bytes
    }
}

impl Piece {
    const PIECE_LEAD: usize = size_of::<Piece<()>>();

    pub fn index(&self) -> u32 {
        u32::from_be_bytes(self.index)
    }

    pub fn begin(&self) -> u32 {
        u32::from_be_bytes(self.begin)
    }

    pub fn block(&self) -> &[u8] {
        &self.block
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        let length = bytes.len();

        if length < Self::PIECE_LEAD {
            return None;
        }

        let piece = &bytes[..length - Self::PIECE_LEAD] as *const [u8] as *const Self;

        Some(unsafe { &*piece })
    }
}
