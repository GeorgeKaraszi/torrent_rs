pub mod magnet;
pub mod peer;
pub mod peer_messages;
pub mod torrent;
pub mod tracker;
pub mod types;

pub const PEER_ID: &types::PeerId = b"-PC0001-123456701112";

pub fn decode_bencode<T>(data: &[u8]) -> anyhow::Result<T, anyhow::Error>
where
    T: serde::de::DeserializeOwned,
{
    let decoded: serde_bencode::value::Value =
        serde_bencode::from_bytes(data).expect("Deserializing bytes");
    let parsed: serde_json::Value = convert(decoded)?.into();

    Ok(serde_json::from_value::<T>(parsed)?)
}

pub fn convert(value: serde_bencode::value::Value) -> anyhow::Result<serde_json::Value> {
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
