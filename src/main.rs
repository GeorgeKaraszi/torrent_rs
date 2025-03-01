use anyhow::{ensure, Context, Error};
use clap::{Parser, Subcommand};
use codecrafters_bittorrent::magnet::*;
use codecrafters_bittorrent::peer::*;
use codecrafters_bittorrent::peer_messages as pm;
use codecrafters_bittorrent::torrent::{Torrent, TorrentInfo};
use codecrafters_bittorrent::tracker::{TackerRequest, Tracker};
use codecrafters_bittorrent::types::HashId;
use codecrafters_bittorrent::{convert, PEER_ID};
use futures::{SinkExt, StreamExt};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

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
    #[command(name = "magnet_test")]
    MagnetTest {
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

async fn handshake_from_peer(
    info_hash: HashId,
    peer: String,
) -> anyhow::Result<PeerHandshake, Error> {
    let mut connection = TcpStream::connect(peer)
        .await
        .expect("Failed to connect to peer");

    let mut handshake = PeerHandshake::new(info_hash);

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

    let reserved = handshake.reserved.clone();
    let ext_id = reserved[reserved.len() - 1];
    println!("EXT ID: {}", ext_id);
    println!("Reserved: {:?}", reserved);

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
    tracker_response: Tracker,
    piece_index: usize,
) -> anyhow::Result<Vec<u8>> {
    let peer = tracker_response.peers[0].clone();

    let piece_setup: Vec<(usize, HashId)> = vec![(piece_index, torrent.info.pieces[piece_index])];

    let piece_block = download_pieces(peer, &torrent.info, piece_setup).await?;
    // Attempting to resist the urge to clone the data. Seems like it'd otherwise start being expensive long term.
    let block = piece_block.into_iter().next().expect("block exists").1;
    Ok(block)
}

fn calculate_piece_size(torrent_info: &TorrentInfo, piece_index: usize) -> usize {
    if piece_index == torrent_info.pieces.len() - 1 {
        let remainder = torrent_info.length % torrent_info.piece_length;
        if remainder == 0 {
            torrent_info.piece_length
        } else {
            remainder
        }
    } else {
        torrent_info.piece_length
    }
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
    let mut peer = PeerConnection::connect(peer).await?;
    peer.send_handshake(torrent_info.hash(), false).await?;
    peer.send_interested().await?;

    let mut peer_connection = peer.connection.try_lock().expect("connection is locked");

    let mut block_collection: Vec<(usize, Vec<u8>)> = Vec::with_capacity(pieces.len());

    for (idx, piece_hash) in pieces {
        let piece_size = calculate_piece_size(&torrent_info, idx);
        let num_of_blocks = (piece_size + (BLOCK_MAX - 1)) / BLOCK_MAX;

        let mut block_data = Vec::with_capacity(piece_size);

        for block in 0..num_of_blocks {
            let block_size = calculate_block_size(piece_size, block, num_of_blocks);
            let request =
                PeerMessage::request(idx as u32, (block * BLOCK_MAX) as u32, block_size as u32);

            peer_connection
                .send(PeerFrame::Message(request))
                .await
                .context("Sending request message")?;

            let piece = peer_connection
                .next()
                .await
                .expect("Peer returns a piece")
                .context("peer message was invalid")?
                .expect_message();

            ensure!(piece.message_tag == MessageTag::Piece);
            ensure!(!piece.payload.is_empty());

            let piece = Piece::from_bytes(&piece.payload[..])
                .expect("always returning pieces from the peers");

            ensure!(piece.index() as usize == idx);
            ensure!(piece.begin() as usize == block * BLOCK_MAX);
            ensure!(piece.block().len() == block_size);

            block_data.extend(piece.block());
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

async fn call_tests(magnet: String) -> anyhow::Result<()> {
    let mut magnet = Magnet::from_str(magnet.as_str())?;
    let tracker = magnet.discover_peers(PEER_ID).await?;
    let connection = TcpStream::connect(tracker.peers[0].ip_address())
        .await
        .context("Failed to connect to peer")?;

    let mut peer = Framed::new(connection, pm::PeerCodec::new());

    let handshake = pm::PeerHandshake::new(Some(magnet.info_hash), None, true);
    peer.send(handshake).await.context("Sending handshake")?;

    let peer_handshake = peer
        .next()
        .await
        .expect("peer always sends handshake")?
        .expect_handshake()?;

    ensure!(peer_handshake.protocol_length == 19);
    ensure!(&peer_handshake.protocol == b"BitTorrent protocol");
    ensure!(&peer_handshake.peer_id != PEER_ID);

    let bitfield = peer
        .next()
        .await
        .expect("peer always sends bitfields")
        .context("peer message was invalid")?
        .unwrap_message();

    ensure!(bitfield.message_tag == pm::MessageTag::Bitfield);

    let ext_handshake = pm::ExtensionHandshakeMessage::default();
    let ext_request = pm::ExtensionPayload::new(0, ext_handshake).into_peer_message();

    peer.send(ext_request)
        .await
        .context("Sending extension handshake")?;

    let ext_response = peer
        .next()
        .await
        .expect("peer always sends extension response")
        .context("peer message was invalid")?
        .expect_extension_message::<pm::ExtensionHandshakeMessage>()?;

    let metadata_id = ext_response.message.payload.metadata.ut_metadata;

    ensure!(metadata_id != 0, "metadata id is not 0");

    ensure!(ext_response.message_tag == pm::MessageTag::Extension);

    let ext_request =
        pm::ExtensionPayload::new(metadata_id, pm::ExtensionPieceRequestMessage::default())
            .into_peer_message();

    peer.send(ext_request)
        .await
        .context("Sending extension request")?;

    let ext_response = peer
        .next()
        .await
        .expect("peer always sends extension response")
        .context("peer message was invalid")?
        .expect_extension_message::<pm::ExtensionPieceRequestMessage>()?;

    ensure!(ext_response.message_tag == pm::MessageTag::Extension);

    let torrent_info = ext_response.message.payload.torrent_info.unwrap();


    println!("Peer ID: {}", hex::encode(peer_handshake.peer_id));
    println!("Peer Metadata Extension ID: {}", metadata_id);
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
            let mut magnet = Magnet::from_str(magnet.as_str())?;
            magnet.handshake(PEER_ID).await?;
        }
        Command::MagnetInfo { magnet } => {
            call_tests(magnet).await?;
            // let mut magnet = Magnet::from_str(magnet.as_str())?;
            // magnet.request_info(PEER_ID).await?;
        }
        Command::MagnetTest { magnet } => {
            call_tests(magnet).await?;
        }
    }

    Ok(())
}
