use anyhow::ensure;
use clap::{Parser, Subcommand};
use codecrafters_bittorrent::magnet::*;
use codecrafters_bittorrent::peer::*;
use codecrafters_bittorrent::messages::PeerHandshake;
use codecrafters_bittorrent::torrent::{Torrent, TorrentInfo};
use codecrafters_bittorrent::tracker::{TackerRequest, Tracker};
use codecrafters_bittorrent::types::{HashId, Result};
use codecrafters_bittorrent::{convert, PEER_ID};
use futures::{SinkExt, StreamExt};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::PathBuf;

const BLOCK_MAX: usize = 1 << 14;

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
    Download(DownloadFile),
    #[command(name = "magnet_parse")]
    MagnetParse {
        magnet: String,
    },
    #[command(name = "magnet_handshake")]
    MagnetHandshake {
        magnet: String,
    },
    #[command(name = "magnet_info")]
    MagnetInfo {
        magnet: String,
    },
}

#[derive(clap::Args, Debug)]
struct DownloadPiece {
    #[arg(short, long)] // Allows both `-o` and `--output`
    output: Option<PathBuf>,
    torrent: PathBuf,
    piece_index: usize,
}

#[derive(clap::Args, Debug)]
struct DownloadFile {
    #[arg(short, long)] // Allows both `-o` and `--output`
    output: PathBuf,
    torrent: PathBuf,
}

async fn fetch_tracker_info(torrent: &Torrent) -> anyhow::Result<Tracker> {
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
    let response = serde_bencode::from_bytes::<Tracker>(&response)?;

    Ok(response)
}

// Note: This test for CodeCrafters doesn't send bitfield messages during testing.
// Hence, why PeerConnection::send_handshake doesn't work.
async fn handshake_from_peer(info_hash: HashId, peer: String) -> Result<()> {
    let peer = Peer::new(peer.as_str());
    let peer = PeerConnection::connect(&peer).await?;
    let mut connection = peer.connection.lock().unwrap();

    connection
        .send(PeerHandshake::new(Some(info_hash), None, false))
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

    println!("Peer ID: {}", hex::encode(handshake.peer_id));
    Ok(())
}

fn read_torrent_file(torrent: PathBuf) -> Result<Torrent> {
    let file = std::fs::read(torrent)?;
    let torrent = serde_bencode::from_bytes::<Torrent>(&file.as_slice())?;
    Ok(torrent)
}

async fn download_peer_piece(
    torrent: Torrent,
    tracker_response: Tracker,
    piece_index: usize,
) -> Result<Vec<u8>> {
    let peer = tracker_response.peers[0].clone();

    let piece_setup: Vec<(usize, HashId)> = vec![(piece_index, torrent.info.pieces[piece_index])];

    let piece_block = download_pieces(peer, &torrent.info, piece_setup).await?;
    // Attempting to resist the urge to clone the data. Seems like it'd otherwise start being expensive long term.
    let block = piece_block.into_iter().next().expect("block exists").1;
    Ok(block)
}

fn calculate_block_size(piece_size: usize, block_index: usize, num_of_blocks: usize) -> usize {
    if block_index == num_of_blocks - 1 {
        let remainder = piece_size % BLOCK_MAX;
        if remainder == 0 {
            BLOCK_MAX
        } else {
            remainder
        }
    } else {
        BLOCK_MAX
    }
}

async fn download_pieces(
    peer: Peer,
    torrent_info: &TorrentInfo,
    pieces: Vec<(usize, HashId)>,
) -> anyhow::Result<Vec<(usize, Vec<u8>)>> {
    let mut peer = PeerConnection::connect(&peer).await?;
    peer.send_handshake(torrent_info.hash(), false).await?;
    peer.send_interest().await?;

    let mut block_collection: Vec<(usize, Vec<u8>)> = Vec::with_capacity(pieces.len());

    for (idx, piece_hash) in pieces {
        let piece_size = torrent_info.calculate_piece_size(idx);
        let num_of_blocks = (piece_size + (BLOCK_MAX - 1)) / BLOCK_MAX;

        let mut block_data = Vec::with_capacity(piece_size);

        for block in 0..num_of_blocks {
            let block_size = calculate_block_size(piece_size, block, num_of_blocks);

            let piece = peer
                .download_piece(idx as u32, (block * BLOCK_MAX) as u32, block_size as u32)
                .await?;

            block_data.extend_from_slice(piece.message.block());
        }

        {
            let mut hasher = Sha1::new();
            hasher.update(&block_data);
            let hasher: HashId = hasher.finalize().try_into().expect("hashing failed");
            ensure!(hasher == piece_hash);
        }

        block_collection.push((idx, block_data));
    }

    Ok(block_collection)
}

async fn download_file(torrent: Torrent, tracker: Tracker) -> anyhow::Result<Vec<u8>> {
    let piece_count = torrent.info.pieces.len();
    let peer_count = tracker.peers.len();
    let mut piece_distribution: HashMap<usize, Vec<(usize, HashId)>> = HashMap::new();

    for (peer_idx, i) in (0..peer_count).cycle().zip(0..piece_count) {
        piece_distribution
            .entry(peer_idx)
            .or_insert(vec![])
            .push((i, torrent.info.pieces[i]));
    }

    let mut async_pieces = Vec::with_capacity(peer_count);

    for (peer_idx, pieces) in piece_distribution {
        let peer = tracker.peers[peer_idx].clone();
        let async_piece = download_pieces(peer, &torrent.info, pieces);
        async_pieces.push(async_piece);
    }

    let block_pieces = futures::future::try_join_all(async_pieces)
        .await?
        .into_iter()
        .flatten()
        .collect::<HashMap<usize, Vec<u8>>>();

    let collected_pieces = (0..torrent.info.pieces.len())
        .filter_map(|i| block_pieces.get(&i))
        .flatten()
        .cloned()
        .collect();

    Ok(collected_pieces)
}

async fn magnet_handshake(magnet: String) -> anyhow::Result<()> {
    let mut magnet = Magnet::from_str(magnet.as_str())?;
    let tracker = magnet.discover_peers(PEER_ID).await?;
    let mut peer = PeerConnection::connect(&tracker.peers[0]).await?;

    peer.send_handshake(magnet.info_hash.clone(), true).await?;
    peer.exchange_magnet_info().await?;

    println!("Peer ID: {}", hex::encode(peer.peer_id()));
    println!("Peer Metadata Extension ID: {}", peer.metadata_id());

    Ok(())
}

async fn magnet_info(magnet: String) -> anyhow::Result<()> {
    let mut magnet = Magnet::from_str(magnet.as_str())?;
    let tracker = magnet.discover_peers(PEER_ID).await?;
    let mut peer = PeerConnection::connect(&tracker.peers[0]).await?;

    peer.send_handshake(magnet.info_hash.clone(), true).await?;
    peer.exchange_magnet_info().await?;

    let requested_info = peer.request_magnet_torrent_info().await?;

    let torrent_info = requested_info.message.payload.torrent_info();

    println!("Peer ID: {}", hex::encode(peer.peer_id()));
    println!("Peer Metadata Extension ID: {}", peer.metadata_id());
    println!("Tracker URL: {}", magnet.announce());
    println!("Length: {}", torrent_info.length);
    println!("Info Hash: {}", hex::encode(magnet.info_hash));
    println!("Piece Length: {}", torrent_info.piece_length);
    println!("Piece Hashes:");
    for hash in torrent_info.pieces.encoded_hashes() {
        println!("{}", hash);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Decode { value } => {
            let value: serde_bencode::value::Value = serde_bencode::from_str(value.as_str())?;
            println!("{}", convert(value)?.to_string());
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
            let output = download_piece.output.unwrap();
            let torrent = read_torrent_file(download_piece.torrent)?;
            let torrent_response = fetch_tracker_info(&torrent).await?;
            let piece_block_data =
                download_peer_piece(torrent, torrent_response, download_piece.piece_index).await?;

            tokio::fs::write(output.clone(), piece_block_data)
                .await
                .expect("writing block piece");

            println!(
                "Piece {} downloaded to: {}",
                download_piece.piece_index,
                output.display()
            );
        }
        Command::Download(download_file_opts) => {
            println!("Download: {:#?}", download_file_opts);
            let output = download_file_opts.output;
            let torrent = read_torrent_file(download_file_opts.torrent)?;
            let torrent_response = fetch_tracker_info(&torrent).await?;
            let raw_file_data = download_file(torrent, torrent_response).await?;

            tokio::fs::write(output.clone(), raw_file_data)
                .await
                .expect("writing file");

            println!("downloaded to: {}", output.display());
        }
        Command::MagnetParse { magnet } => {
            let magnet = Magnet::from_str(magnet.as_str())?;
            println!("Tracker URL: {}", magnet.announce());
            println!("Info Hash: {}", hex::encode(magnet.info_hash))
        }
        Command::MagnetHandshake { magnet } => {
            magnet_handshake(magnet).await?;
        }
        Command::MagnetInfo { magnet } => {
            magnet_info(magnet).await?;
        }
    }

    Ok(())
}
