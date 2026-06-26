//! Minimal Ethereum value-transfer support: RLP encoding, legacy EIP-155
//! transaction signing, and a small JSON-RPC client.
//!
//! We deliberately hand-roll RLP and signing (over `secp256k1` + `keccak`,
//! the same crates `atlas-payload-provider` already uses) rather than pulling
//! in a full Ethereum library, keeping the dependency surface small. The
//! signing path is checked against the canonical EIP-155 test vector in the
//! unit tests below.

use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

use crate::util::{decode_fixed_hex, keccak256, prefixed_hex};

// ---------------------------------------------------------------------------
// RLP encoding
// ---------------------------------------------------------------------------

/// RLP-encode a byte string.
pub fn rlp_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return vec![bytes[0]];
    }
    let mut out = rlp_length_prefix(bytes.len(), 0x80);
    out.extend_from_slice(bytes);
    out
}

/// RLP-encode a list whose items are already RLP-encoded.
pub fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let mut payload = Vec::new();
    for item in items {
        payload.extend_from_slice(item);
    }
    let mut out = rlp_length_prefix(payload.len(), 0xc0);
    out.extend_from_slice(&payload);
    out
}

/// RLP-encode an unsigned integer as its minimal big-endian representation.
pub fn rlp_uint(value: u128) -> Vec<u8> {
    rlp_bytes(&minimal_be(value))
}

fn rlp_length_prefix(len: usize, short_base: u8) -> Vec<u8> {
    if len <= 55 {
        vec![short_base + len as u8]
    } else {
        let len_bytes = minimal_be(len as u128);
        let mut out = Vec::with_capacity(1 + len_bytes.len());
        out.push(short_base + 55 + len_bytes.len() as u8);
        out.extend_from_slice(&len_bytes);
        out
    }
}

/// Minimal big-endian byte encoding of an integer (empty for zero).
fn minimal_be(value: u128) -> Vec<u8> {
    if value == 0 {
        return Vec::new();
    }
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[first..].to_vec()
}

// ---------------------------------------------------------------------------
// Signing
// ---------------------------------------------------------------------------

/// Fields of a legacy (EIP-155) value transfer.
#[derive(Clone, Debug)]
pub struct LegacyTransfer {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_price: u128,
    pub gas_limit: u64,
    pub to: [u8; 20],
    pub value: u128,
}

/// A signed transaction ready for `eth_sendRawTransaction`.
#[derive(Clone, Debug)]
pub struct SignedTransaction {
    pub raw: String,
    pub hash: String,
}

#[derive(Clone)]
pub struct FaucetSigner {
    secret_key: SecretKey,
    address: [u8; 20],
}

impl std::fmt::Debug for FaucetSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaucetSigner")
            .field("address", &self.address_hex())
            .finish_non_exhaustive()
    }
}

impl FaucetSigner {
    pub fn from_private_key_hex(value: &str) -> Result<Self, String> {
        let bytes = decode_fixed_hex(value, 32)?;
        let array: [u8; 32] = bytes
            .try_into()
            .expect("decode_fixed_hex returned exactly 32 bytes");
        let secret_key = SecretKey::from_byte_array(array)
            .map_err(|error| format!("not a valid secp256k1 private key: {error}"))?;
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);
        Ok(Self {
            secret_key,
            address: address_for_public_key(&public_key),
        })
    }

    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    pub fn address_hex(&self) -> String {
        prefixed_hex(&self.address)
    }

    /// Sign a legacy value-transfer transaction with EIP-155 replay protection.
    pub fn sign_legacy_transfer(&self, tx: &LegacyTransfer) -> SignedTransaction {
        let signing_payload = legacy_signing_payload(tx);
        let signing_hash = keccak256(&signing_payload);

        let secp = Secp256k1::new();
        let signature =
            secp.sign_ecdsa_recoverable(Message::from_digest(signing_hash), &self.secret_key);
        let (recovery_id, compact) = signature.serialize_compact();
        let recovery_byte: i32 = recovery_id.into();
        let v = tx.chain_id * 2 + 35 + recovery_byte as u64;

        let r = &compact[..32];
        let s = &compact[32..];
        let signed = rlp_list(&[
            rlp_uint(tx.nonce as u128),
            rlp_uint(tx.gas_price),
            rlp_uint(tx.gas_limit as u128),
            rlp_bytes(&tx.to),
            rlp_uint(tx.value),
            rlp_bytes(&[]),
            rlp_uint(v as u128),
            rlp_bytes(strip_leading_zeros(r)),
            rlp_bytes(strip_leading_zeros(s)),
        ]);

        let hash = keccak256(&signed);
        SignedTransaction {
            raw: prefixed_hex(&signed),
            hash: prefixed_hex(&hash),
        }
    }
}

/// RLP payload that is keccak-hashed to produce the EIP-155 signing hash.
fn legacy_signing_payload(tx: &LegacyTransfer) -> Vec<u8> {
    rlp_list(&[
        rlp_uint(tx.nonce as u128),
        rlp_uint(tx.gas_price),
        rlp_uint(tx.gas_limit as u128),
        rlp_bytes(&tx.to),
        rlp_uint(tx.value),
        rlp_bytes(&[]),
        rlp_uint(tx.chain_id as u128),
        rlp_bytes(&[]),
        rlp_bytes(&[]),
    ])
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

fn address_for_public_key(public_key: &PublicKey) -> [u8; 20] {
    let serialized = public_key.serialize_uncompressed();
    let hash = keccak256(&serialized[1..]);
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..]);
    address
}

// ---------------------------------------------------------------------------
// RLP decoding + sender recovery (used by tests and the mock RPC)
// ---------------------------------------------------------------------------

/// Decoded fields of a signed legacy transaction.
#[derive(Clone, Debug)]
pub struct DecodedLegacyTransfer {
    pub from: [u8; 20],
    pub to: [u8; 20],
    pub value: u128,
    pub nonce: u64,
    pub chain_id: u64,
}

/// Decode and ECDSA-recover the sender of a signed legacy transaction. Returns
/// an error for malformed input or anything other than a 9-field legacy tx.
pub fn recover_legacy_transfer(raw_hex: &str) -> Result<DecodedLegacyTransfer, String> {
    let raw = crate::util::decode_flexible_hex(raw_hex)?;
    let items = rlp_decode_flat_list(&raw)?;
    if items.len() != 9 {
        return Err(format!("expected 9 legacy fields, got {}", items.len()));
    }

    let nonce = be_to_u64(&items[0]);
    let gas_price = be_to_u128(&items[1]);
    let gas_limit = be_to_u64(&items[2]);
    if items[3].len() != 20 {
        return Err("`to` is not a 20-byte address".to_string());
    }
    let mut to = [0u8; 20];
    to.copy_from_slice(&items[3]);
    let value = be_to_u128(&items[4]);
    let data = &items[5];
    let v = be_to_u64(&items[6]);

    if v < 35 {
        return Err("not an EIP-155 signed transaction".to_string());
    }
    let chain_id = (v - 35) / 2;
    let recovery = ((v - 35) % 2) as i32;

    let signing_payload = rlp_list(&[
        rlp_uint(nonce as u128),
        rlp_uint(gas_price),
        rlp_uint(gas_limit as u128),
        rlp_bytes(&to),
        rlp_uint(value),
        rlp_bytes(data),
        rlp_uint(chain_id as u128),
        rlp_bytes(&[]),
        rlp_bytes(&[]),
    ]);
    let signing_hash = keccak256(&signing_payload);

    let mut compact = [0u8; 64];
    left_pad_into(&items[7], &mut compact[..32]);
    left_pad_into(&items[8], &mut compact[32..]);
    let recovery_id =
        RecoveryId::try_from(recovery).map_err(|error| format!("invalid recovery id: {error}"))?;
    let recoverable = RecoverableSignature::from_compact(&compact, recovery_id)
        .map_err(|error| format!("invalid signature: {error}"))?;
    let secp = Secp256k1::new();
    let public_key = secp
        .recover_ecdsa(Message::from_digest(signing_hash), &recoverable)
        .map_err(|error| format!("recovery failed: {error}"))?;

    Ok(DecodedLegacyTransfer {
        from: address_for_public_key(&public_key),
        to,
        value,
        nonce,
        chain_id,
    })
}

/// Decode an RLP list whose every element is a byte string (no nested lists).
fn rlp_decode_flat_list(input: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let (prefix, _) = input.split_first().ok_or("empty RLP input")?;
    let prefix = *prefix;
    if prefix < 0xc0 {
        return Err("RLP top-level value is not a list".to_string());
    }
    let (payload_start, payload_len) = if prefix <= 0xf7 {
        (1usize, (prefix - 0xc0) as usize)
    } else {
        let len_of_len = (prefix - 0xf7) as usize;
        if input.len() < 1 + len_of_len {
            return Err("truncated RLP list length".to_string());
        }
        (1 + len_of_len, be_to_usize(&input[1..1 + len_of_len]))
    };
    let end = payload_start + payload_len;
    if input.len() < end {
        return Err("truncated RLP list payload".to_string());
    }

    let mut items = Vec::new();
    let mut cursor = payload_start;
    while cursor < end {
        let (item, next) = rlp_decode_string(input, cursor)?;
        items.push(item);
        cursor = next;
    }
    Ok(items)
}

/// Decode a single RLP byte string starting at `pos`; returns the bytes and the
/// index just past it.
fn rlp_decode_string(input: &[u8], pos: usize) -> Result<(Vec<u8>, usize), String> {
    let prefix = *input.get(pos).ok_or("truncated RLP item")?;
    if prefix < 0x80 {
        Ok((vec![prefix], pos + 1))
    } else if prefix <= 0xb7 {
        let len = (prefix - 0x80) as usize;
        let start = pos + 1;
        let end = start + len;
        let slice = input.get(start..end).ok_or("truncated RLP short string")?;
        Ok((slice.to_vec(), end))
    } else if prefix <= 0xbf {
        let len_of_len = (prefix - 0xb7) as usize;
        let len_start = pos + 1;
        let len_end = len_start + len_of_len;
        let len = be_to_usize(input.get(len_start..len_end).ok_or("truncated RLP length")?);
        let start = len_end;
        let end = start + len;
        let slice = input.get(start..end).ok_or("truncated RLP long string")?;
        Ok((slice.to_vec(), end))
    } else {
        Err("nested RLP lists are not supported by this decoder".to_string())
    }
}

fn be_to_u64(bytes: &[u8]) -> u64 {
    let mut acc = 0u64;
    for &b in bytes {
        acc = (acc << 8) | b as u64;
    }
    acc
}

fn be_to_u128(bytes: &[u8]) -> u128 {
    let mut acc = 0u128;
    for &b in bytes {
        acc = (acc << 8) | b as u128;
    }
    acc
}

fn be_to_usize(bytes: &[u8]) -> usize {
    be_to_u64(bytes) as usize
}

fn left_pad_into(src: &[u8], dst: &mut [u8]) {
    let offset = dst.len().saturating_sub(src.len());
    dst[offset..].copy_from_slice(src);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Private key from the EIP-155 specification example.
    const EIP155_KEY: &str =
        "0x4646464646464646464646464646464646464646464646464646464646464646";
    const EIP155_SENDER: &str = "0x9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f";

    #[test]
    fn rlp_encodes_known_values() {
        assert_eq!(rlp_uint(0), vec![0x80]);
        assert_eq!(rlp_uint(0x7f), vec![0x7f]);
        assert_eq!(rlp_uint(0x80), vec![0x81, 0x80]);
        assert_eq!(rlp_uint(1024), vec![0x82, 0x04, 0x00]);
        // "dog"
        assert_eq!(rlp_bytes(b"dog"), vec![0x83, b'd', b'o', b'g']);
        // ["cat","dog"]
        assert_eq!(
            rlp_list(&[rlp_bytes(b"cat"), rlp_bytes(b"dog")]),
            vec![0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g']
        );
    }

    #[test]
    fn signs_eip155_official_vector() {
        let signer = FaucetSigner::from_private_key_hex(EIP155_KEY).unwrap();
        let tx = LegacyTransfer {
            chain_id: 1,
            nonce: 9,
            gas_price: 20_000_000_000,
            gas_limit: 21_000,
            to: super::super::util::parse_address("0x3535353535353535353535353535353535353535")
                .unwrap(),
            value: 1_000_000_000_000_000_000,
        };

        // Signing hash from EIP-155.
        let payload = legacy_signing_payload(&tx);
        assert_eq!(
            prefixed_hex(&keccak256(&payload)),
            "0xdaf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53"
        );

        let signed = signer.sign_legacy_transfer(&tx);
        // Canonical signed transaction from EIP-155.
        assert_eq!(
            signed.raw,
            "0xf86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83"
        );

        let decoded = recover_legacy_transfer(&signed.raw).unwrap();
        assert_eq!(prefixed_hex(&decoded.from), EIP155_SENDER);
        assert_eq!(decoded.to, tx.to);
        assert_eq!(decoded.value, tx.value);
        assert_eq!(decoded.nonce, tx.nonce);
        assert_eq!(decoded.chain_id, tx.chain_id);
    }

    #[test]
    fn round_trips_a_large_chain_id_transfer() {
        let signer = FaucetSigner::from_private_key_hex(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap();
        assert_eq!(
            signer.address_hex(),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );

        let tx = LegacyTransfer {
            chain_id: 1337,
            nonce: 7,
            gas_price: 1_000_000_000,
            gas_limit: 21_000,
            to: super::super::util::parse_address("0x70997970C51812dc3A010C7d01b50e0d17dc79C8")
                .unwrap(),
            value: 1_000_000_000_000_000_000,
        };
        let signed = signer.sign_legacy_transfer(&tx);
        let decoded = recover_legacy_transfer(&signed.raw).unwrap();
        assert_eq!(decoded.from, signer.address());
        assert_eq!(decoded.to, tx.to);
        assert_eq!(decoded.value, tx.value);
        assert_eq!(decoded.chain_id, 1337);
    }
}
