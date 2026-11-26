//! Content addressing.
//!
//! Every identity in flot is a digest over a canonical byte sequence. Canonical
//! JSON here means: object keys sorted lexicographically, no insignificant
//! whitespace, UTF-8. Two hosts that observe the same facts must produce the
//! same digest, so no wall-clock value is ever allowed into a digest body.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Digest of raw bytes, rendered as `sha256:<hex>`.
pub fn bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{:x}", hasher.finalize())
}

/// HMAC-SHA256 (RFC 2104) of `message` under `key`, rendered as hex. It
/// lets a host recognise records it wrote itself, which a plain digest
/// cannot: anyone can recompute a digest.
pub fn mac(key: &[u8; 32], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    padded[..key.len()].copy_from_slice(key);
    let mut inner = Sha256::new();
    inner.update(padded.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(padded.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    format!("{:x}", outer.finalize())
}

/// Digest over canonical JSON of a serializable value.
pub fn canonical<T: Serialize>(value: &T) -> String {
    bytes(canonical_json(value).as_bytes())
}

/// Canonical JSON text of a serializable value: the exact bytes `canonical`
/// digests, so storing them stores an object named by that digest.
pub fn canonical_json<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).expect("value is JSON-serializable");
    canonical_bytes(&json)
}

/// Canonical JSON text: recursively key-sorted, compact.
pub fn canonical_bytes(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Fold an ordered list of digests into one digest. Used for the run root.
pub fn fold(label: &str, digests: &[String]) -> String {
    let mut body = Map::new();
    body.insert("label".into(), Value::String(label.to_string()));
    body.insert(
        "members".into(),
        Value::Array(digests.iter().cloned().map(Value::String).collect()),
    );
    bytes(canonical_bytes(&Value::Object(body)).as_bytes())
}

/// Short form for human output. Keeps the algorithm prefix out of the way.
pub fn short(digest: &str) -> String {
    let hex = digest
        .split_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or(digest);
    hex.chars().take(12).collect()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Bundles must stay readable by any JSON tool,
/// and step output is arbitrary bytes, so text is not an option.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let bits = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(bits >> 18) as usize & 63] as char);
        out.push(ALPHABET[(bits >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[bits as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The bytes standard base64 `text` with padding stands for, whitespace
/// ignored.
pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    let value = |symbol: u8| -> Result<u32> {
        ALPHABET
            .iter()
            .position(|candidate| *candidate == symbol)
            .map(|position| position as u32)
            .with_context(|| format!("`{}` is not a base64 symbol", symbol as char))
    };
    let symbols: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !symbols.len().is_multiple_of(4) {
        bail!("base64 length {} is not a multiple of four", symbols.len());
    }
    let mut out = Vec::with_capacity(symbols.len() / 4 * 3);
    for quad in symbols.chunks(4) {
        let padding = quad.iter().filter(|symbol| **symbol == b'=').count();
        let bits = (value(quad[0])? << 18)
            | (value(quad[1])? << 12)
            | (if padding > 1 { 0 } else { value(quad[2])? } << 6)
            | (if padding > 0 { 0 } else { value(quad[3])? });
        out.push((bits >> 16) as u8);
        if padding < 2 {
            out.push((bits >> 8) as u8);
        }
        if padding < 1 {
            out.push(bits as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_matches_rfc_4231_style_vectors() {
        // HMAC-SHA256 with a 32-byte key of 0x0b and "Hi There", checked
        // against Python's hmac module.
        assert_eq!(
            mac(&[0x0b; 32], b"Hi There"),
            "198a607eb44bfbc69903a0f1cf2bbdc5ba0aa3f3d9ae3c1c7a3b1696a0b68cf7"
        );
    }
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_the_digest() {
        let left = json!({ "a": 1, "b": { "x": true, "y": [1, 2] } });
        let right = json!({ "b": { "y": [1, 2], "x": true }, "a": 1 });
        assert_eq!(canonical(&left), canonical(&right));
    }

    #[test]
    fn array_order_does_change_the_digest() {
        assert_ne!(canonical(&json!([1, 2])), canonical(&json!([2, 1])));
    }

    #[test]
    fn fold_is_order_sensitive_and_label_scoped() {
        let a = "sha256:aa".to_string();
        let b = "sha256:bb".to_string();
        assert_ne!(
            fold("run", &[a.clone(), b.clone()]),
            fold("run", &[b.clone(), a.clone()])
        );
        assert_ne!(fold("run", std::slice::from_ref(&a)), fold("other", &[a]));
    }

    #[test]
    fn short_strips_the_algorithm_prefix() {
        assert_eq!(short("sha256:0123456789abcdef"), "0123456789ab");
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        let cases: [&[u8]; 7] = [b"", b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"];
        for data in cases {
            assert_eq!(
                base64_decode(&base64_encode(data)).unwrap(),
                data,
                "{data:?}"
            );
        }
        let binary: Vec<u8> = (0u8..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&binary)).unwrap(), binary);
    }

    #[test]
    fn base64_matches_known_encodings() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }
}
