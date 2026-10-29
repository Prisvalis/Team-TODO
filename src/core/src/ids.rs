use crate::db::Runtime;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// ULID: 48-bit millisecond timestamp + 80 random bits, Crockford base32 (26 chars).
pub fn new_id(rt: &impl Runtime) -> String {
    let mut random = [0u8; 10];
    rt.fill_random(&mut random);
    encode_ulid(rt.now_ms(), random)
}

fn encode_ulid(ms: i64, random: [u8; 10]) -> String {
    let mut value: u128 = (ms as u128 & 0xFFFF_FFFF_FFFF) << 80;
    for (i, byte) in random.iter().enumerate() {
        value |= u128::from(*byte) << (8 * (9 - i));
    }
    (0..26)
        .rev()
        .map(|i| CROCKFORD[((value >> (5 * i)) & 0x1F) as usize] as char)
        .collect()
}

/// 32 random bytes as lowercase hex.
pub fn new_token(rt: &impl Runtime) -> String {
    let mut bytes = [0u8; 32];
    rt.fill_random(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the database stores for a session token: its SHA-256, so a leaked database or
/// backup cannot be replayed as cookies.
pub fn token_key(token: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Rejects ids that could not have come from `new_id`, before they reach SQL.
pub fn is_valid_id(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| CROCKFORD.contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_layout() {
        let id = encode_ulid(0x0192_0000_0000, [0xFF; 10]);
        assert_eq!(id.len(), 26);
        assert!(is_valid_id(&id));
        assert!(id.ends_with("ZZZZZZZZZZZZZZZZ"), "{id}");
        // Later timestamps sort after earlier ones.
        assert!(encode_ulid(2, [0; 10]) > encode_ulid(1, [0xFF; 10]));
    }

    #[test]
    fn token_key_is_sha256_hex() {
        assert_eq!(
            token_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_foreign_ids() {
        assert!(!is_valid_id("01abcdefghjkmnpqrstvwxyz01"));
        assert!(!is_valid_id("short"));
    }
}
