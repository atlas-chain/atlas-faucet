//! Client proof-of-work: stateless HMAC-signed challenges plus a multi-puzzle
//! sha256 leading-zero search.
//!
//! ## Why many small puzzles
//!
//! Instead of one high-difficulty target (whose solve time has huge variance
//! and gives no honest progress signal), a challenge asks the client to solve
//! `puzzles` independent sub-puzzles, each requiring `bits` leading zero bits.
//! This:
//!
//! * parallelises cleanly across CPU cores (each worker takes a slice of the
//!   puzzle indices), giving the "~5 s of multicore CPU" cost;
//! * yields a smooth, *deterministic* progress bar (solved / total puzzles);
//! * is cheap to verify on the server (`puzzles` hashes, one per nonce).
//!
//! The per-puzzle preimage binds the work to the requesting address so a
//! solution cannot be precomputed or reused for a different recipient, and the
//! challenge envelope is HMAC-signed so the server stays stateless about issued
//! challenges (it only remembers *consumed* ones, to stop replay).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::util::{constant_time_eq, decode_fixed_hex, hex_lower, leading_zero_bits, prefixed_hex};

const ALGORITHM: &str = "sha256-leading-zeros";
const VERSION: u32 = 1;
const MAC_DOMAIN: &str = "atlas-faucet-pow";

/// The challenge envelope exchanged with the client (camelCase JSON on the
/// wire). The same struct is echoed back inside a claim so the server can
/// re-derive and check the HMAC without server-side challenge storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub version: u32,
    pub algorithm: String,
    pub address: String,
    pub salt: String,
    pub bits: u32,
    pub puzzles: u32,
    pub issued_at: u64,
    pub expires_at: u64,
    pub hmac: String,
}

/// Parsed + authenticated view of a challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedChallenge {
    pub salt: [u8; 16],
    pub address: [u8; 20],
    pub bits: u32,
    pub puzzles: u32,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChallengeError {
    Malformed(String),
    Unsupported(String),
    Tampered,
    Expired,
}

impl std::fmt::Display for ChallengeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChallengeError::Malformed(message) => write!(f, "malformed challenge: {message}"),
            ChallengeError::Unsupported(message) => write!(f, "unsupported challenge: {message}"),
            ChallengeError::Tampered => write!(f, "challenge HMAC does not match its contents"),
            ChallengeError::Expired => write!(f, "challenge has expired"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowError {
    WrongLength { expected: u32, got: usize },
    PuzzleUnsolved { index: u32 },
}

impl std::fmt::Display for PowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowError::WrongLength { expected, got } => {
                write!(f, "expected {expected} nonces, got {got}")
            }
            PowError::PuzzleUnsolved { index } => {
                write!(f, "proof-of-work puzzle {index} is unsolved")
            }
        }
    }
}

/// Holds the HMAC secret and the small amount of mutable state the faucet keeps
/// to prevent replay (consumed salts) and to enforce per-address cooldown.
pub struct PowKeeper {
    secret: Vec<u8>,
    /// Random per-process value folded into every challenge HMAC. Because the
    /// consumed-salt set is in-memory and forgotten on restart, mixing this in
    /// means challenges issued before a restart fail verification afterwards,
    /// closing the post-restart replay window even when `POW_HMAC_SECRET` is
    /// fixed.
    boot_nonce: [u8; 16],
    consumed: Mutex<HashMap<[u8; 16], u64>>,
    last_claim: Mutex<HashMap<[u8; 20], u64>>,
}

impl PowKeeper {
    pub fn new(secret: Vec<u8>) -> Self {
        Self {
            secret,
            boot_nonce: random_array(),
            consumed: Mutex::new(HashMap::new()),
            last_claim: Mutex::new(HashMap::new()),
        }
    }

    /// Issue a fresh challenge bound to `address`, valid for `ttl_secs` seconds.
    pub fn issue(
        &self,
        address: &[u8; 20],
        bits: u32,
        puzzles: u32,
        ttl_secs: u64,
        now: u64,
    ) -> Challenge {
        let salt: [u8; 16] = random_array();
        let issued_at = now;
        let expires_at = now.saturating_add(ttl_secs);
        let address_hex = prefixed_hex(address);
        let salt_hex = hex_lower(&salt);
        let mac = self.mac(&address_hex, &salt_hex, bits, puzzles, issued_at, expires_at);

        Challenge {
            version: VERSION,
            algorithm: ALGORITHM.to_string(),
            address: address_hex,
            salt: prefixed_hex(&salt),
            bits,
            puzzles,
            issued_at,
            expires_at,
            hmac: prefixed_hex(&mac),
        }
    }

    /// Authenticate a challenge envelope and parse its fields.
    pub fn verify_challenge(
        &self,
        challenge: &Challenge,
        now: u64,
    ) -> Result<VerifiedChallenge, ChallengeError> {
        if challenge.version != VERSION {
            return Err(ChallengeError::Unsupported(format!(
                "version {}",
                challenge.version
            )));
        }
        if challenge.algorithm != ALGORITHM {
            return Err(ChallengeError::Unsupported(format!(
                "algorithm {}",
                challenge.algorithm
            )));
        }

        let salt_bytes = decode_fixed_hex(&challenge.salt, 16)
            .map_err(|error| ChallengeError::Malformed(format!("salt: {error}")))?;
        let address_bytes = crate::util::parse_address(&challenge.address)
            .map_err(|error| ChallengeError::Malformed(format!("address: {error}")))?;
        let provided_mac = decode_fixed_hex(&challenge.hmac, 32)
            .map_err(|error| ChallengeError::Malformed(format!("hmac: {error}")))?;

        // Re-derive the MAC over the normalized fields. Note we use the
        // canonical lower-case address / salt rendering, not the raw strings,
        // so cosmetic re-casing cannot break verification while any change to a
        // signed value still does.
        let address_hex = prefixed_hex(&address_bytes);
        let salt_hex = hex_lower(&salt_bytes);
        let expected_mac = self.mac(
            &address_hex,
            &salt_hex,
            challenge.bits,
            challenge.puzzles,
            challenge.issued_at,
            challenge.expires_at,
        );
        if !constant_time_eq(&provided_mac, &expected_mac) {
            return Err(ChallengeError::Tampered);
        }

        if now > challenge.expires_at {
            return Err(ChallengeError::Expired);
        }

        let mut salt = [0u8; 16];
        salt.copy_from_slice(&salt_bytes);
        Ok(VerifiedChallenge {
            salt,
            address: address_bytes,
            bits: challenge.bits,
            puzzles: challenge.puzzles,
            expires_at: challenge.expires_at,
        })
    }

    fn mac(
        &self,
        address_hex: &str,
        salt_hex: &str,
        bits: u32,
        puzzles: u32,
        issued_at: u64,
        expires_at: u64,
    ) -> [u8; 32] {
        let boot = hex_lower(&self.boot_nonce);
        let message = format!(
            "{MAC_DOMAIN}|v{VERSION}|{ALGORITHM}|{boot}|{address_hex}|{salt_hex}|{bits}|{puzzles}|{issued_at}|{expires_at}"
        );
        hmac_sha256(&self.secret, message.as_bytes())
    }

    /// Atomically mark a salt as consumed. Returns `true` if this call consumed
    /// it (i.e. it was previously unused), `false` on replay. Expired entries
    /// are pruned opportunistically.
    pub fn consume_salt(&self, salt: &[u8; 16], expires_at: u64, now: u64) -> bool {
        let mut guard = self.consumed.lock().expect("consumed mutex poisoned");
        guard.retain(|_, exp| *exp >= now);
        if guard.contains_key(salt) {
            return false;
        }
        guard.insert(*salt, expires_at);
        true
    }

    /// Release a previously-consumed salt so the same solved challenge can be
    /// retried. Only called when a claim fails *before* any transaction was
    /// broadcast, so it cannot reopen a replay window.
    pub fn release_salt(&self, salt: &[u8; 16]) {
        self.consumed
            .lock()
            .expect("consumed mutex poisoned")
            .remove(salt);
    }

    /// Remaining cooldown in seconds for `address` (0 when ready). Read-only;
    /// used by the challenge endpoint to reject early. `cooldown_secs == 0`
    /// disables the cooldown entirely.
    pub fn cooldown_remaining(&self, address: &[u8; 20], cooldown_secs: u64, now: u64) -> u64 {
        if cooldown_secs == 0 {
            return 0;
        }
        let guard = self.last_claim.lock().expect("last_claim mutex poisoned");
        match guard.get(address) {
            Some(&last) => last.saturating_add(cooldown_secs).saturating_sub(now),
            None => 0,
        }
    }

    /// Atomically reserve a claim slot for `address`. In one critical section it
    /// checks the cooldown *and* records `now`, so two concurrent claims for the
    /// same address cannot both pass (the second sees the just-inserted entry).
    /// Returns `Err(remaining)` when the address is still cooling down. When the
    /// cooldown is disabled this is a no-op success (operator opted out of any
    /// per-address limit). The recorded entry doubles as an in-flight lock until
    /// the claim succeeds (kept) or fails ([`Self::release_claim`]).
    pub fn try_begin_claim(
        &self,
        address: &[u8; 20],
        cooldown_secs: u64,
        now: u64,
    ) -> Result<(), u64> {
        if cooldown_secs == 0 {
            return Ok(());
        }
        let mut guard = self.last_claim.lock().expect("last_claim mutex poisoned");
        guard.retain(|_, last| last.saturating_add(cooldown_secs) >= now);
        if let Some(&last) = guard.get(address) {
            let remaining = last.saturating_add(cooldown_secs).saturating_sub(now);
            if remaining > 0 {
                return Err(remaining);
            }
        }
        guard.insert(*address, now);
        Ok(())
    }

    /// Undo a reservation made by [`Self::try_begin_claim`] (call when the claim
    /// fails after reserving), so a failed dispense does not lock the address
    /// out for a full cooldown. Only clears the entry if it still matches the
    /// timestamp we reserved.
    pub fn release_claim(&self, address: &[u8; 20], reserved_at: u64) {
        let mut guard = self.last_claim.lock().expect("last_claim mutex poisoned");
        if guard.get(address) == Some(&reserved_at) {
            guard.remove(address);
        }
    }
}

/// Per-puzzle preimage hash: `sha256(salt || address || k_le || nonce_le)`.
/// Must match the browser solver byte-for-byte.
pub fn puzzle_hash(salt: &[u8; 16], address: &[u8; 20], index: u32, nonce: u32) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(address);
    hasher.update(index.to_le_bytes());
    hasher.update(nonce.to_le_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Verify every sub-puzzle nonce meets the difficulty.
pub fn verify_solution(
    challenge: &VerifiedChallenge,
    nonces: &[u32],
) -> Result<(), PowError> {
    // A zero-puzzle (or zero-bit) challenge would otherwise be trivially
    // satisfiable; reject it defensively even though config validation prevents
    // such challenges from being issued.
    if challenge.puzzles == 0 || challenge.bits == 0 {
        return Err(PowError::PuzzleUnsolved { index: 0 });
    }
    if nonces.len() != challenge.puzzles as usize {
        return Err(PowError::WrongLength {
            expected: challenge.puzzles,
            got: nonces.len(),
        });
    }
    for (index, &nonce) in nonces.iter().enumerate() {
        let hash = puzzle_hash(&challenge.salt, &challenge.address, index as u32, nonce);
        if leading_zero_bits(&hash) < challenge.bits {
            return Err(PowError::PuzzleUnsolved {
                index: index as u32,
            });
        }
    }
    Ok(())
}

/// Reference solver for a single sub-puzzle (used by tests and the parity
/// harness; the production solver lives in the browser).
pub fn solve_puzzle(salt: &[u8; 16], address: &[u8; 20], bits: u32, index: u32) -> u32 {
    let mut nonce = 0u32;
    loop {
        let hash = puzzle_hash(salt, address, index, nonce);
        if leading_zero_bits(&hash) >= bits {
            return nonce;
        }
        nonce = nonce.checked_add(1).expect("nonce space exhausted");
    }
}

/// HMAC-SHA256 (RFC 2104), hand-rolled over the `sha2` crate to avoid an extra
/// dependency. Verified against RFC 4231 test vectors.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        block_key[..digest.len()].copy_from_slice(&digest);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] ^= block_key[index];
        opad[index] ^= block_key[index];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    let outer_digest = outer.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&outer_digest);
    out
}

fn random_array<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).expect("OS RNG unavailable");
    buf
}

/// Generate a random HMAC secret (used when none is configured).
pub fn random_secret() -> Vec<u8> {
    random_array::<32>().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> [u8; 20] {
        crate::util::parse_address("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266").unwrap()
    }

    #[test]
    fn hmac_matches_rfc4231_case2() {
        // RFC 4231 Test Case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex_lower(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn issue_then_verify_roundtrips() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let challenge = keeper.issue(&addr(), 8, 4, 120, 1_000);
        let verified = keeper.verify_challenge(&challenge, 1_050).unwrap();
        assert_eq!(verified.address, addr());
        assert_eq!(verified.bits, 8);
        assert_eq!(verified.puzzles, 4);
    }

    #[test]
    fn tampering_with_any_signed_field_is_rejected() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let base = keeper.issue(&addr(), 8, 4, 120, 1_000);

        let mut more_money = base.clone();
        more_money.puzzles = 1; // try to make the work easier
        assert_eq!(
            keeper.verify_challenge(&more_money, 1_050),
            Err(ChallengeError::Tampered)
        );

        let mut longer = base.clone();
        longer.expires_at += 10_000;
        assert_eq!(
            keeper.verify_challenge(&longer, 1_050),
            Err(ChallengeError::Tampered)
        );

        let mut other_addr = base.clone();
        other_addr.address = "0x0000000000000000000000000000000000000001".to_string();
        assert_eq!(
            keeper.verify_challenge(&other_addr, 1_050),
            Err(ChallengeError::Tampered)
        );
    }

    #[test]
    fn expired_challenge_is_rejected() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let challenge = keeper.issue(&addr(), 8, 4, 120, 1_000);
        assert_eq!(
            keeper.verify_challenge(&challenge, 2_000),
            Err(ChallengeError::Expired)
        );
    }

    #[test]
    fn solve_and_verify_full_challenge() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let challenge = keeper.issue(&addr(), 10, 6, 120, 1_000);
        let verified = keeper.verify_challenge(&challenge, 1_010).unwrap();

        let nonces: Vec<u32> = (0..verified.puzzles)
            .map(|k| solve_puzzle(&verified.salt, &verified.address, verified.bits, k))
            .collect();
        assert!(verify_solution(&verified, &nonces).is_ok());

        // A single wrong nonce fails verification.
        let mut bad = nonces.clone();
        bad[2] = bad[2].wrapping_add(1);
        assert!(matches!(
            verify_solution(&verified, &bad),
            Err(PowError::PuzzleUnsolved { index: 2 })
        ));

        // Wrong length fails.
        assert!(matches!(
            verify_solution(&verified, &nonces[..nonces.len() - 1]),
            Err(PowError::WrongLength { .. })
        ));
    }

    #[test]
    fn browser_parity_vector() {
        // Vector produced by running the *shipped* browser worker under Node
        // (`node scripts/pow-parity.mjs`). Pins JS↔Rust proof-of-work parity:
        // identical preimage layout, sha256, and leading-zero predicate.
        let salt: [u8; 16] = std::array::from_fn(|i| i as u8);
        let address: [u8; 20] = std::array::from_fn(|i| (i + 0x10) as u8);

        // Single-hash parity for a fixed (index, nonce).
        let hash = puzzle_hash(&salt, &address, 3, 123_456);
        assert_eq!(
            hex_lower(&hash),
            "cb61395fc2a014e274c4e5af81101378b3d979e9052582fb9746cd6e900582c1"
        );

        // The nonces the browser solver found must verify in Rust at 12 bits.
        let bits = 12;
        let nonces: [u32; 8] = [283, 868, 12068, 8201, 9499, 2143, 4847, 511];
        for (index, &nonce) in nonces.iter().enumerate() {
            let hash = puzzle_hash(&salt, &address, index as u32, nonce);
            assert!(
                leading_zero_bits(&hash) >= bits,
                "browser nonce for puzzle {index} fails Rust verification"
            );
        }
    }

    #[test]
    fn consume_salt_blocks_replay() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let salt = [7u8; 16];
        assert!(keeper.consume_salt(&salt, 2_000, 1_000));
        assert!(!keeper.consume_salt(&salt, 2_000, 1_000));
    }

    #[test]
    fn cooldown_tracks_last_claim() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let a = addr();
        assert_eq!(keeper.cooldown_remaining(&a, 60, 1_000), 0);
        assert_eq!(keeper.try_begin_claim(&a, 60, 1_000), Ok(()));
        assert_eq!(keeper.cooldown_remaining(&a, 60, 1_030), 30);
        assert_eq!(keeper.cooldown_remaining(&a, 60, 1_070), 0);
        // Disabled cooldown is always ready.
        assert_eq!(keeper.cooldown_remaining(&a, 0, 1_000), 0);
    }

    #[test]
    fn try_begin_claim_serializes_same_address() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let a = addr();
        // First claim reserves the slot.
        assert_eq!(keeper.try_begin_claim(&a, 60, 1_000), Ok(()));
        // A concurrent second claim for the same address is rejected.
        assert_eq!(keeper.try_begin_claim(&a, 60, 1_000), Err(60));
        // Releasing the reservation (failed dispense) frees the address.
        keeper.release_claim(&a, 1_000);
        assert_eq!(keeper.try_begin_claim(&a, 60, 1_000), Ok(()));
        // Disabled cooldown never blocks.
        assert_eq!(keeper.try_begin_claim(&a, 0, 1_000), Ok(()));
    }

    #[test]
    fn release_salt_allows_retry() {
        let keeper = PowKeeper::new(b"test-secret".to_vec());
        let salt = [9u8; 16];
        assert!(keeper.consume_salt(&salt, 2_000, 1_000));
        assert!(!keeper.consume_salt(&salt, 2_000, 1_000));
        keeper.release_salt(&salt);
        assert!(keeper.consume_salt(&salt, 2_000, 1_000));
    }

    #[test]
    fn boot_nonce_invalidates_cross_instance_challenges() {
        // Two keepers with the SAME secret still reject each other's challenges,
        // modelling a restart with a fixed POW_HMAC_SECRET.
        let a = addr();
        let k1 = PowKeeper::new(b"shared-secret".to_vec());
        let k2 = PowKeeper::new(b"shared-secret".to_vec());
        let challenge = k1.issue(&a, 8, 4, 120, 1_000);
        assert!(k1.verify_challenge(&challenge, 1_010).is_ok());
        assert_eq!(
            k2.verify_challenge(&challenge, 1_010),
            Err(ChallengeError::Tampered)
        );
    }
}
