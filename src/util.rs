//! Small hex / hashing helpers shared across the faucet.
//!
//! These mirror the minimal-dependency style used elsewhere in the Atlas
//! stack (see `atlas-payload-provider/src/signer.rs`): we hand-roll hex
//! handling instead of pulling in a `hex` crate.

use sha3::{Digest, Keccak256};

/// Lowercase, unprefixed hex encoding.
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Lowercase `0x`-prefixed hex encoding.
pub fn prefixed_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(2 + bytes.len() * 2);
    output.push_str("0x");
    output.push_str(&hex_lower(bytes));
    output
}

/// Decode a hex string body (no `0x` prefix, even length) into bytes.
pub fn decode_hex_body(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(format!("odd number of hex characters: {}", hex.len()));
    }
    let chars = hex.as_bytes();
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for index in (0..chars.len()).step_by(2) {
        let chunk = std::str::from_utf8(&chars[index..index + 2])
            .map_err(|error| format!("invalid utf8 in hex string: {error}"))?;
        let byte = u8::from_str_radix(chunk, 16)
            .map_err(|error| format!("invalid hex byte at index {index}: {error}"))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Decode a hex string that may carry an optional `0x`/`0X` prefix.
pub fn decode_flexible_hex(value: &str) -> Result<Vec<u8>, String> {
    let trimmed = value.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    decode_hex_body(body)
}

/// Decode an exactly `expected_bytes`-long hex value (optional `0x` prefix).
pub fn decode_fixed_hex(value: &str, expected_bytes: usize) -> Result<Vec<u8>, String> {
    let bytes = decode_flexible_hex(value)?;
    if bytes.len() != expected_bytes {
        return Err(format!(
            "expected {expected_bytes} bytes, got {}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

/// keccak-256 over `data`.
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    output
}

/// Count the number of leading zero *bits* across `hash`, scanning from the
/// most-significant bit of `hash[0]`. Used by the proof-of-work check and
/// must match the browser solver exactly.
pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut count = 0u32;
    for &byte in hash {
        if byte == 0 {
            count += 8;
        } else {
            count += byte.leading_zeros();
            break;
        }
    }
    count
}

/// Constant-time byte comparison (avoids leaking equality timing for MACs).
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Parse a `0x`-prefixed 20-byte Ethereum address (any case) into bytes.
pub fn parse_address(value: &str) -> Result<[u8; 20], String> {
    let trimmed = value.trim();
    if !(trimmed.starts_with("0x") || trimmed.starts_with("0X")) {
        return Err("address must be 0x-prefixed".to_string());
    }
    let bytes = decode_fixed_hex(trimmed, 20)?;
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// EIP-55 checksummed string form of a 20-byte address.
pub fn checksum_address(address: &[u8; 20]) -> String {
    let lower = hex_lower(address);
    let hash = keccak256(lower.as_bytes());
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (index, ch) in lower.chars().enumerate() {
        if ch.is_ascii_digit() {
            out.push(ch);
        } else {
            // Upper-case the hex letter when the corresponding nibble of the
            // keccak hash of the lowercase address has its high bit set.
            let nibble = if index % 2 == 0 {
                hash[index / 2] >> 4
            } else {
                hash[index / 2] & 0x0f
            };
            if nibble >= 8 {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = [0x00u8, 0x01, 0xab, 0xff];
        assert_eq!(hex_lower(&bytes), "0001abff");
        assert_eq!(prefixed_hex(&bytes), "0x0001abff");
        assert_eq!(decode_flexible_hex("0x0001abff").unwrap(), bytes);
        assert_eq!(decode_flexible_hex("0001ABFF").unwrap(), bytes);
    }

    #[test]
    fn leading_zero_bits_counts_correctly() {
        assert_eq!(leading_zero_bits(&[0xff]), 0);
        assert_eq!(leading_zero_bits(&[0x00, 0xff]), 8);
        assert_eq!(leading_zero_bits(&[0x00, 0x00, 0x80]), 16);
        assert_eq!(leading_zero_bits(&[0x0f]), 4);
        assert_eq!(leading_zero_bits(&[0x00, 0x01]), 15);
        assert_eq!(leading_zero_bits(&[0x00, 0x00, 0x00, 0x00]), 32);
    }

    #[test]
    fn parse_address_validates_length_and_prefix() {
        assert!(parse_address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").is_ok());
        assert!(parse_address("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266").is_err());
        assert!(parse_address("0x1234").is_err());
    }

    #[test]
    fn checksum_address_matches_eip55_vector() {
        // Canonical EIP-55 example address.
        let addr = parse_address("0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed").unwrap();
        assert_eq!(
            checksum_address(&addr),
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed"
        );
    }

    #[test]
    fn constant_time_eq_behaves() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
