use anyhow::{ensure, Context, Error};
use clap::{Parser, Subcommand};
use codecrafters_bittorrent::peer::*;
use codecrafters_bittorrent::torrent::Torrent;
use codecrafters_bittorrent::tracker::{TackerRequest, TrackerResponse};
use futures::{SinkExt, StreamExt};
use serde_json;
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

const PEER_ID: &[u8; 20] = b"-PC0001-123456701112";
const BLOCK_MAX: usize = 1 << 14;
// const BLOCK_MAX: u32 = 16 * 1024;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Decode {
        value: String,
    },
    Info {
        torrent: PathBuf,
    },
    Peers {
        torrent: PathBuf,
    },
    Handshake {
        torrent: PathBuf,
        peer: String,
    },
    #[command(name = "download_piece")]
    DownloadPiece(DownloadPiece),
}

#[derive(clap::Args, Debug)] // `Args` is required to parse fields properly
struct DownloadPiece {
    #[arg(short, long)] // Allows both `-o` and `--output`
    output: Option<PathBuf>,
    torrent: PathBuf,
    piece_index: usize,
}

fn decode_bencoded_value(encoded_value: String) -> anyhow::Result<serde_json::Value> {
    let value: serde_bencode::value::Value = serde_bencode::from_str(encoded_value.as_str())?;
    convert(value)
}

fn convert(value: serde_bencode::value::Value) -> anyhow::Result<serde_json::Value> {
    match value {
        serde_bencode::value::Value::Int(n) => Ok(n.into()),
        serde_bencode::value::Value::Bytes(s) => Ok(String::from_utf8(s)?.to_string().into()),
        serde_bencode::value::Value::List(list) => {
            let mut vec = vec![];
            for value in list {
                vec.push(convert(value)?);
            }
            Ok(vec.into())
        }
        serde_bencode::value::Value::Dict(dict) => {
            let mut dictionary = serde_json::Map::new();
            for (key, value) in dict {
                let key = String::from_utf8(key)?;
                let value = convert(value)?;
                dictionary.insert(key, value);
            }

            Ok(dictionary.into())
        }
    }
}

async fn fetch_tracker_info(torrent: &Torrent) -> anyhow::Result<TrackerResponse> {
    let req = TackerRequest {
        peer_id: std::str::from_utf8(PEER_ID)?.to_string(),
        port: 6881,
        uploaded: 0,
        downloaded: 0,
        left: torrent.info.length,
        compact: 1,
    };

    let params = serde_urlencoded::to_string(&req)?;

    let info_hash = torrent.info.encoded_hash();
    let url = format!("{}?{}&info_hash={}", torrent.announce, params, info_hash);

    let response = reqwest::get(url).await.expect("Fetching tracker info");
    let response = response.bytes().await.expect("Reading response");
    let response = serde_bencode::from_bytes::<TrackerResponse>(&response)?;

    Ok(response)
}

async fn handshake_from_peer(
    info_hash: [u8; 20],
    peer: String,
) -> anyhow::Result<PeerHandshake, Error> {
    let mut connection = TcpStream::connect(peer)
        .await
        .expect("Failed to connect to peer");

    let mut handshake = PeerHandshake::new(info_hash, *PEER_ID);

    {
        let handshake_bytes =
            &mut handshake as *mut PeerHandshake as *mut [u8; size_of::<PeerHandshake>()];

        let handshake_bytes: &mut [u8; size_of::<PeerHandshake>()] =
            unsafe { &mut *handshake_bytes };

        connection
            .write_all(handshake_bytes)
            .await
            .context("Write Handshake")?;

        connection
            .read_exact(handshake_bytes)
            .await
            .context("Read Handshake")?;
    }

    println!("Peer ID: {}", hex::encode(handshake.peer_id));

    Ok(handshake)
}

fn read_torrent_file(torrent: PathBuf) -> anyhow::Result<Torrent> {
    let file = std::fs::read(torrent)?;
    let torrent = serde_bencode::from_bytes::<Torrent>(&file.as_slice())?;
    Ok(torrent)
}

async fn download_peer_piece(
    torrent: Torrent,
    tracker_response: TrackerResponse,
    piece_index: usize,
    output: PathBuf,
) -> anyhow::Result<()> {
    let peer = &tracker_response.peers[0];
    let mut peer = TcpStream::connect(peer.ip_address())
        .await
        .context("failed to connect to peer")?;

    let mut handshake = PeerHandshake::new(torrent.info.hash(), *PEER_ID);

    {
        let handshake_bytes = handshake.mut_ptr();
        peer.write_all(handshake_bytes)
            .await
            .context("Write Handshake")?;
        peer.read_exact(handshake_bytes)
            .await
            .context("Read Handshake")?;
    }

    ensure!(handshake.protocol_length == 19);
    ensure!(&handshake.protocol == b"BitTorrent protocol");
    ensure!(&handshake.peer_id != PEER_ID);

    let mut peer = Framed::new(peer, PeerMessageCodec);

    let bitfield = peer
        .next()
        .await
        .expect("peer always sends bitfields")
        .context("peer message was invalid")?;

    ensure!(bitfield.message_tag == MessageTag::Bitfield);

    peer.send(PeerMessage::interested()).await?;

    let unchoked = peer
        .next()
        .await
        .expect("peer responses with unchoked")
        .context("peer message was invalid")?;

    ensure!(unchoked.message_tag == MessageTag::Unchoke);
    ensure!(unchoked.payload.is_empty());

    let piece_hash = torrent.info.pieces[piece_index];
    let piece_size = if piece_index == torrent.info.pieces.len() - 1 {
        let md = torrent.info.length % torrent.info.piece_length;
        if md == 0 {
            torrent.info.piece_length
        } else {
            md
        }
    } else {
        torrent.info.piece_length
    };

    let num_of_blocks = (piece_size + (BLOCK_MAX - 1)) / BLOCK_MAX;
    let mut block_collection: Vec<u8> = Vec::with_capacity(piece_size);

    for block in 0..num_of_blocks {
        let block_size = if block == num_of_blocks - 1 {
            let md = piece_size % BLOCK_MAX;
            if md == 0 {
                BLOCK_MAX
            } else {
                md
            }
        } else {
            BLOCK_MAX
        };

        let mut request = RequestMessage::new(
            piece_index as u32,
            (block * BLOCK_MAX) as u32,
            block_size as u32,
        );

        let request_message = PeerMessage::new(MessageTag::Request, Vec::from(request.mut_ptr()));

        peer.send(request_message)
            .await
            .with_context(|| format!("Sending request message {block}"))?;

        let piece = peer
            .next()
            .await
            .expect("Peer returns a piece")
            .context("peer message was invalid")?;

        ensure!(piece.message_tag == MessageTag::Piece);
        ensure!(!piece.payload.is_empty());

        let piece = Piece::from_bytes(&piece.payload[..])
            .expect("always returning pieces from the peers");

        ensure!(piece.index() as usize == piece_index);
        ensure!(piece.begin() as usize == block * BLOCK_MAX);
        ensure!(piece.block().len() == block_size);

        block_collection.extend(piece.block());
    }

    let mut hasher = Sha1::new();
    hasher.update(&block_collection);
    let hasher: [u8; 20] = hasher.finalize().try_into().expect("hashing failed");
    ensure!(hasher == piece_hash);

    tokio::fs::write(&output, block_collection)
        .await
        .expect("writing block piece");

    println!("Piece {piece_index} downloaded to: {}", output.display());

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Decode { value } => {
            println!("{}", decode_bencoded_value(value)?.to_string());
        }
        Command::Info { torrent } => {
            let torrent = read_torrent_file(torrent)?;
            println!("Tracker URL: {}", torrent.announce);
            println!("Length: {}", torrent.info.length);
            println!("Info Hash: {}", hex::encode(torrent.info.hash()));
            println!("Piece Length: {}", torrent.info.piece_length);
            for hash in torrent.info.pieces.encoded_hashes() {
                println!("{}", hash);
            }
        }
        Command::Peers { torrent } => {
            let torrent = read_torrent_file(torrent)?;
            let torrent_response = fetch_tracker_info(&torrent).await?;
            for peer in torrent_response.peers.iter() {
                println!("{}:{}", peer.ip, peer.port);
            }
        }
        Command::Handshake { torrent, peer } => {
            let torrent = read_torrent_file(torrent)?;
            handshake_from_peer(torrent.info.hash(), peer).await?;
        }
        Command::DownloadPiece(download_piece) => {
            println!("{:?}", download_piece);
            let torrent = read_torrent_file(download_piece.torrent)?;
            let torrent_response = fetch_tracker_info(&torrent).await?;
            download_peer_piece(
                torrent,
                torrent_response,
                download_piece.piece_index,
                download_piece.output.unwrap(),
            )
            .await?;
        }
    }

    Ok(())
}
