use serde_json;
use std::env;

// Available if you need it!
// use serde_bencode

fn decode_bencoded_value(encoded_value: &str) -> anyhow::Result<(serde_json::Value, &str)> {
    match encoded_value.chars().next() {
        // i<number>e
        Some('i') => {
            if let Some((number, rest)) = encoded_value
                .strip_prefix('i')
                .and_then(|rest| rest.split_once('e'))
            {
                let number = number.parse::<i64>()?;
                Ok((number.into(), rest))
            } else {
                panic!("Invalid encoded value {}", encoded_value)
            }
        }
        // [list]
        Some('l') => {
            let mut list = vec![];
            let mut rest = &encoded_value[1..];

            while !rest.is_empty() && rest.chars().next() != Some('e') {
                let (decoded_value, new_rest) = decode_bencoded_value(rest)?;
                rest = new_rest;
                list.push(decoded_value);
            }

            Ok((list.into(), &rest[1..]))
        }
        // <length of str>:<string>
        Some('0'..='9') => {
            if let Some((length, rest)) = encoded_value.split_once(':') {
                let length = length.parse::<usize>()?;
                let string = &rest[..length];
                Ok((string.to_string().into(), &rest[length..]))
            } else {
                panic!("Invalid encoded value {}", encoded_value)
            }
        }
        _ => panic!("Invalid encoded value {}", encoded_value),
    }
}

// fn decode_bencoded_value(encoded_value: &str) -> anyhow::Result<serde_json::Value> {
//     let value: serde_bencode::value::Value = serde_bencode::from_str(encoded_value)?;
//     convert(value)
// }
//
// fn convert(value: serde_bencode::value::Value) -> anyhow::Result<serde_json::Value> {
//     match value {
//         serde_bencode::value::Value::Int(n) => Ok(n.into()),
//         serde_bencode::value::Value::Bytes(s) => Ok(String::from_utf8(s)?.to_string().into()),
//         serde_bencode::value::Value::List(list) => {
//             let mut vec = vec![];
//             for value in list {
//                 vec.push(convert(value)?);
//             }
//             Ok(vec.into())
//         }
//         _ => Err(anyhow::anyhow!("Unhandled encoded value: {:?}", value)),
//     }
// }

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let command = &args[1];

    if command == "decode" {
        // You can use print statements as follows for debugging, they'll be visible when running tests.
        // eprintln!("Logs from your program will appear here!");

        // Uncomment this block to pass the first stage
        let encoded_value = &args[2];
        let decoded_value = decode_bencoded_value(encoded_value)?;
        println!("{}", decoded_value.0.to_string());
    } else {
        println!("unknown command: {}", args[1])
    }

    Ok(())
}
