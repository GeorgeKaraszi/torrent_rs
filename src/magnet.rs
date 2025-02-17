use crate::peer::HashType;
use anyhow::{anyhow, Error, Result};
use hex::FromHex;
use reqwest::Url;
use std::ops::Deref;

#[derive(Debug, Clone)]
pub struct Magnet {
    pub name: Option<String>,
    pub info_hash: HashType,
    pub tracker: Option<String>,
}

impl Magnet {
    pub fn from_str(magnet: &str) -> Result<Self, Error> {
        let mut info_hash = None;
        let mut tracker: Option<String> = None;
        let mut name: Option<String> = None;

        let url = Url::parse(magnet)?;

        for (query_name, value) in url.query_pairs() {
            match query_name.deref() {
                "xt" => match value.deref().strip_prefix("urn:btih:") {
                    Some(info_hash_hex) => {
                        info_hash = Some(HashType::from_hex(info_hash_hex)?);
                    }
                    None => {
                        return Err(anyhow!("invalid info hash"));
                    }
                },
                "dn" => {
                    name = Some(value.into_owned());
                }
                "tr" => {
                    tracker = Some(value.into_owned());
                }
                _ => {}
            }
        }

        Ok(Self {
            name: name,
            info_hash: info_hash.expect("no hash found"),
            tracker: tracker,
        })
    }
}
