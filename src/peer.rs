use crate::messages::*;
use crate::types::{HashId, PeerId, Result};
use crate::PEER_ID;
use anyhow::{Context, Error};
use bytes::{Buf, BytesMut};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Deserializer};
use std::fmt::Debug;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio_util::codec::{Decoder, Encoder, Framed};
// ----------------- CONSTANTS & TYPE ALIASES -----------------

const HANDSHAKE_LENGTH: usize = size_of::<PeerHandshake>(); // 68
const MAX_MESSAGE_LENGTH: usize = 1 << 16;

trait SendableMessage {}
impl SendableMessage for PeerHandshake {}
impl<T> SendableMessage for PeerMessage<T> where T: MessageSerialization {}

// ----------------- CORE PEER STRUCTS -----------------
#[derive(Debug, Clone)]
pub struct Peers(Vec<Peer>);

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
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

// ---------------------- PEER  ----------------------

impl Deref for Peers {
    type Target = Vec<Peer>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Peers {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
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
    pub fn new(ip_address: &str) -> Self {
        let mut parts = ip_address.split(':');
        let ip = parts.next().unwrap().parse().unwrap();
        let port = parts.next().unwrap().parse().unwrap();

        Self { ip, port }
    }

    pub fn ip_address(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}

// ----------------- PEER CONNECTION -----------------

impl PeerConnection {
    pub async fn connect(peer: &Peer) -> Result<Self> {
        let addr = peer.ip_address();
        let stream = TcpStream::connect(addr).await?;
        let codec = PeerCodec::new();
        let framed = Framed::new(stream, codec);

        Ok(Self {
            peer: peer.clone(),
            peer_id: None,
            metadata_id: None,
            connection: Arc::new(Mutex::new(framed)),
        })
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id.expect("peer id not set")
    }

    pub fn metadata_id(&self) -> u8 {
        self.metadata_id.expect("metadata id not set")
    }

    pub fn requires_handshake(&self) -> bool {
        self.peer_id.is_none()
    }

    pub fn requires_extension_exchange(&self) -> bool {
        self.requires_handshake() || self.metadata_id.is_none()
    }

    pub async fn send_handshake(
        &mut self,
        info_hash: HashId,
        extension: bool,
    ) -> Result<PeerHandshake> {
        let mut connection = self.connection.lock().unwrap();

        connection
            .send(PeerHandshake::new(Some(info_hash), None, extension))
            .await?;

        let handshake = connection
            .next()
            .await
            .expect("peer sending handshake")?
            .expect_handshake()?;

        anyhow::ensure!(&handshake.peer_id != PEER_ID);
        anyhow::ensure!(handshake.protocol_length == 19, "Invalid protocol length");
        anyhow::ensure!(
            &handshake.protocol == b"BitTorrent protocol",
            "Invalid protocol"
        );

        let bitfield = connection
            .next()
            .await
            .expect("peer always submits bitfield after handshake")
            .context("peer bitfield message is invalid")?
            .unwrap_message();

        anyhow::ensure!(
            bitfield.message_tag == MessageTag::Bitfield,
            "Invalid bitfield message"
        );

        self.peer_id = Some(handshake.peer_id.clone());

        Ok(handshake)
    }

    pub async fn send_interest(&mut self) -> Result<PeerMessageBuffer> {
        let mut connection = self.connection.lock().unwrap();

        connection
            .send(PeerMessage::new(MessageTag::Interested, vec![]))
            .await?;

        let message: PeerMessageBuffer = connection
            .next()
            .await
            .expect("peer responses with unchoked message")
            .context("peer unchoked message is invalid")?
            .expect_message()?;

        anyhow::ensure!(message.message_tag == MessageTag::Unchoke);
        anyhow::ensure!(message.message.is_empty());
        Ok(message)
    }

    pub async fn download_piece(
        &mut self,
        index: u32,
        begin: u32,
        length: u32,
    ) -> Result<PeerMessage<PieceMessage>> {
        let mut connection = self.connection.lock().unwrap();
        let request = PieceRequestMessage::new(index, begin, length);

        connection
            .send(PeerMessage::new(MessageTag::Request, request.clone()))
            .await
            .context("Sending piece request message")?;

        let piece_message: PeerMessage<PieceMessage> = connection
            .next()
            .await
            .expect("peer always sends extension response")
            .context("peer message was invalid")?
            .expect_message()?;

        anyhow::ensure!(piece_message.message_tag == MessageTag::Piece);
        anyhow::ensure!(piece_message.message.index() == request.index());
        anyhow::ensure!(piece_message.message.begin() == request.begin());
        anyhow::ensure!(piece_message.message.length() == request.length());

        Ok(piece_message)
    }

    pub async fn exchange_magnet_info(
        &mut self,
    ) -> Result<PeerExtMessageBuffer<ExtensionHandshakeMessage>> {
        let mut connection = self.connection.lock().unwrap();

        let peer_ext_message =
            ExtensionPayload::new(0, ExtensionHandshakeMessage::default()).into_peer_message();

        connection
            .send(peer_ext_message)
            .await
            .context("Sending extension handshake")?;

        let handshake_extension = connection
            .next()
            .await
            .expect("peer always sends extension response")
            .context("peer message was invalid")?
            .expect_extension_message::<ExtensionHandshakeMessage>()?;

        anyhow::ensure!(handshake_extension.message_tag == MessageTag::Extension);

        self.metadata_id = Some(handshake_extension.message.payload.metadata.ut_metadata);

        Ok(handshake_extension)
    }

    pub async fn request_magnet_torrent_info(
        &mut self,
    ) -> Result<PeerExtMessageBuffer<ExtensionPieceRequestMessage>> {
        let mut connection = self.connection.lock().unwrap();

        let extension_payload =
            ExtensionPayload::new(self.metadata_id(), ExtensionPieceRequestMessage::default())
                .into_peer_message();

        connection
            .send(extension_payload)
            .await
            .context("Sending ExtensionPieceRequestMessage message")?;

        let extension_message = connection
            .next()
            .await
            .expect("peer always sends extension response")
            .context("peer message was invalid")?
            .expect_extension_message::<ExtensionPieceRequestMessage>()?;

        anyhow::ensure!(extension_message.message_tag == MessageTag::Extension);

        Ok(extension_message)
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

                let handshake =
                    <PeerHandshake as MessageSerialization>::deserialize(&src[..HANDSHAKE_LENGTH])?;
                src.advance(HANDSHAKE_LENGTH);

                self.state = CodecState::Messages;
                Ok(Some(PeerFrame::Handshake(handshake)))
            }
            CodecState::Messages => {
                if src.len() < 4 {
                    return Ok(None);
                }

                let length = u32::from_be_bytes(src[..4].try_into().unwrap()) as usize;
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
