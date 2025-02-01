mod tracker;

use clap::{Parser, Subcommand};
use codecrafters_bittorrent::PiecesHashes;
use serde::{Deserialize, Serialize};
use serde_json;
use sha1::digest::Output;
use sha1::{Digest, Sha1, Sha1Core};
use std::path::PathBuf;
// use serde_bencode::value::Value;
use crate::tracker::TorrentResponse;
use tracker::TorrentRequest;

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
}

#[derive(serde::Deserialize, Debug)]
struct Torrent {
    announce: String,
    info: TorrentInfo,
}

#[derive(Serialize, Deserialize, Debug)]
struct TorrentInfo {
    name: String,
    #[serde(rename = "piece length")]
    piece_length: usize,
    pieces: PiecesHashes,
    length: usize,
}

impl TorrentInfo {
    fn hasher(&self) -> Output<Sha1Core> {
        let mut hasher = Sha1::new();
        Digest::update(&mut hasher, serde_bencode::to_bytes(&self).unwrap());
        hasher.finalize()
    }
    fn hash(&self) -> String {
        hex::encode(&self.hasher())
    }

    fn url_hash(&self) -> String {
        let hashed = self.hasher();
        let mut encoded = String::with_capacity(hashed.len() * 3);
        for byte in hashed.iter() {
            encoded.push('%');
            encoded.push_str(hex::encode(&[*byte]).as_str());
        }

        encoded
    }
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

async fn fetch_tracker_info(torrent: &Torrent) -> anyhow::Result<TorrentResponse> {
    let req = TorrentRequest {
        peer_id: "-PC0001-123456700012".to_string(),
        port: 6881,
        uploaded: 0,
        downloaded: 0,
        left: torrent.info.length,
        compact: 1,
    };

    let params = serde_urlencoded::to_string(&req)?;

    let info_hash = torrent.info.url_hash();
    let url = format!("{}?{}&info_hash={}", torrent.announce, params, info_hash);

    let response = reqwest::get(url).await.expect("Fetching tracker info");
    let response = response.bytes().await.expect("Reading response");
    let response = serde_bencode::from_bytes::<TorrentResponse>(&response)?;

    println!("{:?}", response);
    Ok(response)
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
            println!("Info Hash: {}", torrent.info.hash());
            println!("Piece Length: {}", torrent.info.piece_length);
            for hash in torrent.info.pieces.hashes() {
                println!("{}", hash);
            }
        }
        Command::Peers { torrent } => {
            let torrent = read_torrent_file(torrent)?;
            let torrent_response = fetch_tracker_info(&torrent).await?;
            for peer in torrent_response.peers.iter() {
                println!("{}", peer.ip_address());
            }
        }
    }

    Ok(())
}
