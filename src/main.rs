use serde_json;
use std::env;
use serde::{Serialize, Deserialize};
use sha1::{Sha1, Digest};
use codecrafters_bittorrent::PiecesHashes;

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
    fn hash(&self) -> String {
        let mut hasher = Sha1::new();
        hasher.update(serde_bencode::to_bytes(&self).unwrap());
        format!("{:x}", hasher.finalize())
    }
}

fn decode_bencoded_value(encoded_value: &str) -> anyhow::Result<serde_json::Value> {
    let value: serde_bencode::value::Value = serde_bencode::from_str(encoded_value)?;
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

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let command = &args[1];

    if command == "decode" {
        // Uncomment this block to pass the first stage
        let encoded_value = &args[2];
        let decoded_value = decode_bencoded_value(encoded_value)?;
        println!("{}", decoded_value.to_string());
    } else if command == "info" {
        let file_path = &args[2];
        let file = std::fs::read(file_path)?;
        let torrent = serde_bencode::from_bytes::<Torrent>(&file.as_slice())?;
        println!("Tracker URL: {}", torrent.announce);
        println!("Length: {}", torrent.info.length);
        println!("Info Hash: {}", torrent.info.hash());
    } else {
        println!("unknown command: {}", args[1])
    }

    Ok(())
}
