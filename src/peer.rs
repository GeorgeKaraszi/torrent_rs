use anyhow::{anyhow, ensure, Context, Error};
use bytes::{Buf, BufMut, BytesMut};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Deserializer};
use std::net::{IpAddr, Ipv4Addr};
use std::ops::{Deref, DerefMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Decoder, Encoder, Framed};

const PEER_ID: &[u8; 20] = b"-PC0001-123456701112";

#[derive(Debug, Clone)]
pub struct Peers(Vec<Peer>);

#[derive(Debug, Clone)]
pub struct Peer {
    pub ip: IpAddr,
    pub port: u16,
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct PeerHandshake {
    pub protocol_length: u8,
    pub protocol: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
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

pub struct PeerConnection {
    pub peer: Peer,
    pub connection: Framed<TcpStream, PeerMessageCodec>,
}

#[repr(C)]
#[derive(Debug)]
pub struct PeerMessage {
    pub message_tag: MessageTag,
    pub payload: Vec<u8>,
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
    UNKNOWN = 9,
}

pub struct PeerMessageCodec;

const MAX: usize = 1 << 16;
impl Decoder for PeerMessageCodec {
    type Item = PeerMessage;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
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

        if length > MAX {
            return Err(Error::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large", length),
            )));
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
            tag => {
                return Err(anyhow!("Unknown message tag id: {}", tag));
            }
        };

        let message_payload = if src.len() > 5 {
            src[5..4 + length].to_vec()
        } else {
            Vec::new()
        };

        // let message_payload: Vec<u8> = match src.get(..=message_length) {
        //     Some(payload) => payload.to_vec(),
        //     None => Vec::new(),
        // };

        src.advance(4 + length);

        Ok(Some(PeerMessage::new(message_tag, message_payload)))
    }
}

impl Encoder<PeerMessage> for PeerMessageCodec {
    type Error = Error;

    fn encode(&mut self, item: PeerMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let message_length: [u8; 4] = u32::to_be_bytes(item.payload.len() as u32 + 1);
        dst.put_slice(&message_length);
        dst.put_slice(item.to_bytes().as_slice());
        Ok(())
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

impl Peer {
    pub fn ip_address(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}

impl PeerConnection {
    pub async fn connect(peer: Peer, info_hash: [u8; 20]) -> anyhow::Result<Self, Error> {
        let mut connection = TcpStream::connect(peer.ip_address())
            .await
            .context("Failed to connect to peer")?;

        let mut handshake = PeerHandshake::new(info_hash, *PEER_ID);

        {
            let handshake_bytes = handshake.mut_ptr();
            connection
                .write_all(handshake_bytes)
                .await
                .context("Write Handshake")?;
            connection
                .read_exact(handshake_bytes)
                .await
                .context("Read Handshake")?;
        }

        ensure!(handshake.protocol_length == 19);
        ensure!(&handshake.protocol == b"BitTorrent protocol");
        ensure!(&handshake.peer_id != PEER_ID);

        let mut connection = Framed::new(connection, PeerMessageCodec);

        let bitfield = connection
            .next()
            .await
            .expect("peer always sends bitfields")
            .context("peer message was invalid")?;

        ensure!(bitfield.message_tag == MessageTag::Bitfield);

        Ok(Self {
            peer: peer,
            connection: connection,
        })
    }

    pub async fn send_interested(&mut self) -> anyhow::Result<(), Error> {
        self.connection.send(PeerMessage::interested()).await?;

        let unchoked = self
            .connection
            .next()
            .await
            .expect("peer responses with unchoked")
            .context("peer message was invalid")?;

        ensure!(unchoked.message_tag == MessageTag::Unchoke);
        ensure!(unchoked.payload.is_empty());

        Ok(())
    }
}

impl PeerMessage {
    pub fn new(message_tag: MessageTag, payload: Vec<u8>) -> Self {
        Self {
            message_tag,
            payload,
        }
    }

    pub fn new_buffer(capacity: Option<usize>) -> Self {
        let capacity = capacity.unwrap_or(1024);
        Self::new(MessageTag::UNKNOWN, Vec::with_capacity(capacity))
    }

    pub fn interested() -> Self {
        Self::new(MessageTag::Interested, vec![])
    }

    pub fn request(index: u32, begin: u32, length: u32) -> Self {
        let request = RequestMessage {
            index: index.to_be_bytes(),
            begin: begin.to_be_bytes(),
            length: length.to_be_bytes(),
        };

        let mut buffer: Vec<u8> = Vec::new();
        buffer.extend_from_slice(&request.index);
        buffer.extend_from_slice(&request.begin);
        buffer.extend_from_slice(&request.length);

        Self::new(MessageTag::Request, buffer)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::with_capacity(1 + self.payload.len());
        bytes.push(self.message_tag as u8);
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    pub fn mut_ptr(&mut self) -> &mut [u8; size_of::<Self>()] {
        let bytes = self as *mut Self as *mut [u8; size_of::<Self>()];
        let bytes: &mut [u8; size_of::<Self>()] = unsafe { &mut *bytes };

        bytes
    }
}

impl Deref for Peers {
    type Target = Vec<Peer>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Peers {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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

impl PeerHandshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            protocol_length: 19,
            protocol: *b"BitTorrent protocol",
            reserved: [0; 8],
            info_hash: info_hash,
            peer_id: peer_id,
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
