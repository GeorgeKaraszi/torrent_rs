use anyhow::{Context, Error};
use clap::{Parser, Subcommand};
use codecrafters_bittorrent::peer::PeerHandshake;
use codecrafters_bittorrent::torrent::Torrent;
use codecrafters_bittorrent::tracker::{TackerRequest, TrackerResponse};
use serde_json;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
const PEER_ID: &[u8; 20] = b"-PC0001-123456701112";

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Decode { value: String },
    Info { torrent: PathBuf },
    Peers { torrent: PathBuf },
    Handshake { torrent: PathBuf, peer: String },
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

async fn handshake_from_peer(torrent: &Torrent, peer: String) -> anyhow::Result<(), Error> {
    let mut connection = TcpStream::connect(peer)
        .await
        .expect("Failed to connect to peer");

    let mut handshake = PeerHandshake::new(torrent.info.hash(), *PEER_ID);

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

    Ok(())
}

fn read_torrent_file(torrent: PathBuf) -> anyhow::Result<Torrent> {
    let file = std::fs::read(torrent)?;
    let torrent = serde_bencode::from_bytes::<Torrent>(&file.as_slice())?;
    Ok(torrent)
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
            handshake_from_peer(&torrent, peer).await?;
        }
    }

    Ok(())
}
