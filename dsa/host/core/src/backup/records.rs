//! The records of a key-value backup, as the agent sends them and the host keeps them.
//!
//! A record is a 32-bit big-endian key length, the key in UTF-8, a 32-bit big-endian value
//! length (-1 for a deleted key) and the value. What the host keeps for a package is the same
//! format with no deletions, one record per key, in key order.

use std::collections::BTreeMap;

/// The longest key Android's backup data allows, in bytes.
pub const MAX_KEY_BYTES: usize = 64 * 1024;

/// One key's change: its new value, or `None` when it was deleted.
pub type Records = Vec<(String, Option<Vec<u8>>)>;

fn take<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    if data.len() < n {
        return Err("the key-value records end in the middle of one".into());
    }
    let (head, rest) = data.split_at(n);
    *data = rest;
    Ok(head)
}

fn take_u32(data: &mut &[u8]) -> Result<u32, String> {
    Ok(u32::from_be_bytes(take(data, 4)?.try_into().expect("4")))
}

/// Reads the records of `data`.
pub fn parse(mut data: &[u8]) -> Result<Records, String> {
    let mut records = Vec::new();
    while !data.is_empty() {
        let key_len = take_u32(&mut data)? as usize;
        if key_len == 0 || key_len > MAX_KEY_BYTES {
            return Err(format!("a key-value record has a key of {key_len} bytes"));
        }
        let key = std::str::from_utf8(take(&mut data, key_len)?)
            .map_err(|_| "a key-value record's key is not UTF-8".to_string())?
            .to_string();
        let value_len = take_u32(&mut data)? as i32;
        let value = match value_len {
            -1 => None,
            n if n >= 0 => Some(take(&mut data, n as usize)?.to_vec()),
            n => return Err(format!("a key-value record has a value of {n} bytes")),
        };
        records.push((key, value));
    }
    Ok(records)
}

/// Writes `map` as records.
pub fn encode(map: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in map {
        out.extend_from_slice(&(key.len() as u32).to_be_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value);
    }
    out
}

/// Writes changes as records, deletions included.
pub fn encode_records(records: &Records) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in records {
        out.extend_from_slice(&(key.len() as u32).to_be_bytes());
        out.extend_from_slice(key.as_bytes());
        match value {
            Some(value) => {
                out.extend_from_slice(&(value.len() as u32).to_be_bytes());
                out.extend_from_slice(value);
            }
            None => out.extend_from_slice(&(-1i32).to_be_bytes()),
        }
    }
    out
}

/// Applies `records` in order to `map`: a value sets its key, a deletion removes it.
pub fn apply(map: &mut BTreeMap<String, Vec<u8>>, records: Records) {
    for (key, value) in records {
        match value {
            Some(value) => {
                map.insert(key, value);
            }
            None => {
                map.remove(&key);
            }
        }
    }
}
