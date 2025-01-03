use serde_json;
use std::env;

// Available if you need it!
// use serde_bencode

#[allow(dead_code)]
fn decode_bencoded_value(encoded_value: &str) -> Result<serde_json::Value, String> {
    // string,integer = "l<digits>:<string>i<digit>e...."
    // example = "l5:helloi52ee"
    if let Some(str) = encoded_value.strip_prefix('l') {
        let mut list: Vec<serde_json::Value> = vec![];
        let mut rest = str;

        while !rest.is_empty() {
            if let Ok(decoded_value) = decode_bencoded_value(rest) {
                rest = &rest[decoded_value.to_string().len()..];
                list.push(decoded_value);
            } else {
                break;
            }
        }

        return Ok(serde_json::Value::Array(list));
    } else {
        // integer = "i<digits>e"
        if let Some(n) = encoded_value
            .strip_prefix('i')
            .and_then(|rest| rest.split_once('e'))
            .and_then(|(digits, _)| digits.parse::<i64>().ok())
        {
            return Ok(serde_json::Value::Number(serde_json::Number::from(n)));
        } else {
            // string = "<length>:<contents>"
            if let Some((len, rest)) = encoded_value.split_once(':') {
                if let Ok(len) = len.parse::<usize>() {
                    return Ok(serde_json::Value::String(rest[..len].to_string()));
                }
            }
        }
    }

    Err(format!("Unhandled encoded value: {}", encoded_value))
}

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() {
    let args: Vec<String> = env::args().collect();
    let command = &args[1];

    if command == "decode" {
        // You can use print statements as follows for debugging, they'll be visible when running tests.
        // eprintln!("Logs from your program will appear here!");

        // Uncomment this block to pass the first stage
        let encoded_value = &args[2];
        let decoded_value = decode_bencoded_value(encoded_value);
        println!("{}", decoded_value.unwrap().to_string());
    } else {
        println!("unknown command: {}", args[1])
    }
}
