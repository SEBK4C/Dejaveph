//! Capability signing for the local-fs `/xorb-data` URL (`Prompt.md` §5.4, §10).
//!
//! Reconstruction hands clients a time-limited, signed URL so the bulk-data path stays off the
//! bearer-auth check: `…/xorb-data/{hash}?exp=<unix>&sig=<mac>`. The MAC is a BLAKE3 keyed hash
//! (HMAC-equivalent) over `{hash}\n{exp}` with a per-process key, so a client can fetch only the
//! exact object the server signed, and only until `exp`. The key never leaves the server.

/// Seconds since the Unix epoch (monotonic enough for a coarse TTL).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn message(hash_hex: &str, exp: u64) -> String {
    format!("xorb-data\n{hash_hex}\n{exp}")
}

/// Sign `(hash, exp)` → lowercase-hex MAC.
pub fn sign(key: &[u8; 32], hash_hex: &str, exp: u64) -> String {
    blake3::keyed_hash(key, message(hash_hex, exp).as_bytes()).to_hex().to_string()
}

/// Verify a capability: unexpired AND the MAC matches (constant-time).
pub fn verify(key: &[u8; 32], hash_hex: &str, exp: u64, sig_hex: &str, now: u64) -> bool {
    if exp < now {
        return false;
    }
    use subtle::ConstantTimeEq;
    let expected = sign(key, hash_hex, exp);
    expected.as_bytes().ct_eq(sig_hex.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrips() {
        let key = [7u8; 32];
        let h = "ab".repeat(32);
        let exp = now_unix() + 900;
        let sig = sign(&key, &h, exp);
        assert!(verify(&key, &h, exp, &sig, now_unix()));
    }

    #[test]
    fn rejects_tamper_expiry_and_wrong_key() {
        let key = [7u8; 32];
        let h = "ab".repeat(32);
        let now = now_unix();
        let exp = now + 900;
        let sig = sign(&key, &h, exp);
        // expired
        assert!(!verify(&key, &h, exp, &sig, exp + 1));
        // tampered sig
        assert!(!verify(&key, &h, exp, &format!("{}0", &sig[..sig.len() - 1]), now));
        // different exp (sig was for the old exp)
        assert!(!verify(&key, &h, exp + 1, &sig, now));
        // wrong key
        assert!(!verify(&[8u8; 32], &h, exp, &sig, now));
        // wrong hash
        assert!(!verify(&key, &"cd".repeat(32), exp, &sig, now));
    }
}
