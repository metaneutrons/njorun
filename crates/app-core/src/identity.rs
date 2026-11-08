//! The tablet's management identity: a random tablet ID and a static X25519 private key.
//!
//! Both are generated once on the tablet from the hardware random number generator and kept
//! until the setup is reset, so Home Assistant can recognize the tablet across restarts and
//! firmware updates. The private key never leaves the tablet; the Noise handshake derives the
//! public key from it. Storage is the firmware's job; this module only creates, encodes and
//! checks the values.

use std::fmt;

/// Random bytes needed for a new identity: 16 for the tablet ID, 32 for the key.
pub const RANDOM_BYTES: usize = 48;

const ID_PREFIX: &str = "njorun-";
const ID_HEX: usize = 32;

/// A tablet's management identity.
#[derive(Clone, PartialEq, Eq)]
pub struct TabletIdentity {
    tablet_id: String,
    private_key: [u8; 32],
}

/// What storage held.
#[derive(Debug, PartialEq, Eq)]
pub enum Stored {
    /// Nothing yet: generate an identity.
    Missing,
    Valid(TabletIdentity),
    /// Present but unusable, or only half of it present.
    Damaged,
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0_u8; N];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// A key whose bytes are all equal (all zero after a wiped or failed write) is not random.
fn plausible_key(key: &[u8; 32]) -> bool {
    key.iter().any(|byte| *byte != key[0])
}

impl TabletIdentity {
    /// Builds a new identity from fresh hardware randomness.
    #[must_use]
    pub fn from_random(random: &[u8; RANDOM_BYTES]) -> Self {
        let mut private_key = [0_u8; 32];
        private_key.copy_from_slice(&random[16..]);
        Self {
            tablet_id: format!("{ID_PREFIX}{}", hex(&random[..16])),
            private_key,
        }
    }

    /// Stable tablet ID, such as `njorun-3f9c…` (39 characters). Not a secret.
    #[must_use]
    pub fn tablet_id(&self) -> &str {
        &self.tablet_id
    }

    #[must_use]
    pub const fn private_key(&self) -> &[u8; 32] {
        &self.private_key
    }

    /// The private key as 64 lowercase hex digits, for storage only.
    #[must_use]
    pub fn encoded_key(&self) -> String {
        hex(&self.private_key)
    }

    /// Whether a random draw can become an identity; all-equal bytes mean the RNG failed.
    #[must_use]
    pub fn random_is_usable(random: &[u8; RANDOM_BYTES]) -> bool {
        random.iter().any(|byte| *byte != random[0])
    }

    /// Checks what storage returned for the tablet ID and the encoded key.
    #[must_use]
    pub fn classify(tablet_id: Option<&str>, encoded_key: Option<&str>) -> Stored {
        match (tablet_id, encoded_key) {
            (None, None) => Stored::Missing,
            (Some(tablet_id), Some(encoded_key)) => {
                let id_ok = tablet_id
                    .strip_prefix(ID_PREFIX)
                    .is_some_and(|rest| unhex::<{ ID_HEX / 2 }>(rest).is_some());
                match unhex::<32>(encoded_key) {
                    Some(private_key) if id_ok && plausible_key(&private_key) => {
                        Stored::Valid(Self {
                            tablet_id: tablet_id.to_owned(),
                            private_key,
                        })
                    }
                    _ => Stored::Damaged,
                }
            }
            _ => Stored::Damaged,
        }
    }
}

impl fmt::Debug for TabletIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TabletIdentity")
            .field("tablet_id", &self.tablet_id)
            .field("private_key", &"***")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{RANDOM_BYTES, Stored, TabletIdentity};

    fn random(seed: u8) -> [u8; RANDOM_BYTES] {
        let mut bytes = [0_u8; RANDOM_BYTES];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = seed.wrapping_add(u8::try_from(index).unwrap_or(0).wrapping_mul(7));
        }
        bytes
    }

    #[test]
    fn a_new_identity_survives_a_storage_round_trip() {
        let identity = TabletIdentity::from_random(&random(3));
        assert_eq!(identity.tablet_id().len(), 39);
        assert!(identity.tablet_id().starts_with("njorun-"));
        assert_eq!(identity.encoded_key().len(), 64);
        assert_eq!(identity.private_key()[0], random(3)[16]);
        let stored =
            TabletIdentity::classify(Some(identity.tablet_id()), Some(&identity.encoded_key()));
        assert_eq!(stored, Stored::Valid(identity));
    }

    #[test]
    fn different_randomness_gives_different_identities() {
        let first = TabletIdentity::from_random(&random(1));
        let second = TabletIdentity::from_random(&random(2));
        assert_ne!(first.tablet_id(), second.tablet_id());
        assert_ne!(first.private_key(), second.private_key());
    }

    #[test]
    fn missing_partial_and_damaged_values_are_told_apart() {
        let identity = TabletIdentity::from_random(&random(9));
        let id = identity.tablet_id();
        let key = identity.encoded_key();
        assert_eq!(TabletIdentity::classify(None, None), Stored::Missing);
        assert_eq!(TabletIdentity::classify(Some(id), None), Stored::Damaged);
        assert_eq!(TabletIdentity::classify(None, Some(&key)), Stored::Damaged);
        let damaged = [
            (id.to_owned(), key[..63].to_owned()),
            (id.to_owned(), key.to_uppercase()),
            (id.to_owned(), "0".repeat(64)),
            (id.replace("njorun-", "other-"), key.clone()),
            (format!("{id}0"), key.clone()),
            (format!("njorun-{}", "g".repeat(32)), key.clone()),
        ];
        for (id, key) in damaged {
            assert_eq!(
                TabletIdentity::classify(Some(&id), Some(&key)),
                Stored::Damaged,
                "{id} {key}"
            );
        }
    }

    #[test]
    fn a_failed_random_draw_is_refused_and_the_key_is_never_printed() {
        assert!(!TabletIdentity::random_is_usable(&[0; RANDOM_BYTES]));
        assert!(!TabletIdentity::random_is_usable(&[0xff; RANDOM_BYTES]));
        assert!(TabletIdentity::random_is_usable(&random(5)));
        let identity = TabletIdentity::from_random(&random(5));
        let printed = format!("{identity:?}");
        assert!(printed.contains(identity.tablet_id()));
        assert!(!printed.contains(&identity.encoded_key()));
        assert!(printed.contains("***"));
    }
}
