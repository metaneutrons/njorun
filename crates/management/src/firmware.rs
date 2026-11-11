//! Verification of signed firmware releases (decision D7), without any transport.
//!
//! Implements the verifier side of `docs/protocols/003-firmware-manifest.md`: the Ed25519
//! signature over the exact manifest bytes first, then strict parsing, key ID, board
//! selection and, while an image streams in, size and SHA-256. The reference implementation is
//! `scripts/release/manifest.py`; both run against `protocol/fixtures/firmware-manifest.json`.
//!
//! The trusted public keys are a build-time setting: [`TrustedKeys::embedded`] reads
//! `protocol/release-keys.json` as it was when this crate was compiled. A fork replaces that
//! file with its own keys.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Largest manifest that is read at all.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Longest signature file (88 base64 characters, optionally followed by a newline).
pub const MAX_SIGNATURE_BYTES: usize = 90;

const RELEASE_KEYS: &str = include_str!("../../../protocol/release-keys.json");

/// The stable reason codes of `docs/protocols/003-firmware-manifest.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Malformed,
    BadSignature,
    UnsupportedFormat,
    KeyIdMismatch,
    BoardMismatch,
    RevisionUnsupported,
    ChannelMismatch,
    SizeMismatch,
    DigestMismatch,
    Oversize,
}

impl Reason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::BadSignature => "bad_signature",
            Self::UnsupportedFormat => "unsupported_format",
            Self::KeyIdMismatch => "key_id_mismatch",
            Self::BoardMismatch => "board_mismatch",
            Self::RevisionUnsupported => "revision_unsupported",
            Self::ChannelMismatch => "channel_mismatch",
            Self::SizeMismatch => "size_mismatch",
            Self::DigestMismatch => "digest_mismatch",
            Self::Oversize => "oversize",
        }
    }
}

impl Display for Reason {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The public keys that verify a manifest: current first, then next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKeys {
    keys: Vec<[u8; 32]>,
}

impl TrustedKeys {
    /// Keys from raw 32-byte public keys.
    #[must_use]
    pub const fn new(keys: Vec<[u8; 32]>) -> Self {
        Self { keys }
    }

    /// Keys from standard padded base64 texts of the raw keys.
    ///
    /// # Errors
    /// [`Reason::Malformed`] when a text is not a 32-byte key.
    pub fn from_base64(texts: &[&str]) -> Result<Self, Reason> {
        texts
            .iter()
            .map(|text| decode_key(text))
            .collect::<Result<Vec<_>, _>>()
            .map(Self::new)
    }

    /// Keys from the contents of `protocol/release-keys.json` (`keys.current`, `keys.next`).
    ///
    /// # Errors
    /// [`Reason::Malformed`] when the file is not in that shape.
    pub fn from_release_keys(json: &str) -> Result<Self, Reason> {
        let value: Value = serde_json::from_str(json).map_err(|_| Reason::Malformed)?;
        let text = |name: &str| value["keys"][name].as_str().ok_or(Reason::Malformed);
        Self::from_base64(&[text("current")?, text("next")?])
    }

    /// The keys this firmware was built with.
    ///
    /// # Errors
    /// [`Reason::Malformed`] when the embedded file is damaged; a test pins that it is not.
    pub fn embedded() -> Result<Self, Reason> {
        Self::from_release_keys(RELEASE_KEYS)
    }

    /// The raw keys, current first.
    #[must_use]
    pub fn keys(&self) -> &[[u8; 32]] {
        &self.keys
    }
}

fn decode_key(text: &str) -> Result<[u8; 32], Reason> {
    let bytes = STANDARD.decode(text).map_err(|_| Reason::Malformed)?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| Reason::Malformed)?;
    VerifyingKey::from_bytes(&key).map_err(|_| Reason::Malformed)?;
    Ok(key)
}

/// The key ID: the first 8 bytes of SHA-256 over the raw key, as 16 lowercase hex characters.
#[must_use]
pub fn key_id(public_key: &[u8; 32]) -> String {
    hex(&Sha256::digest(public_key)[..8])
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// What the tablet asks the manifest for.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub board: &'a str,
    pub hardware_revision: &'a str,
    /// The channel the install command names. Checked when present.
    pub channel: Option<&'a str>,
}

/// An image the manifest describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub name: String,
    pub size: u64,
    /// Lowercase hex SHA-256.
    pub sha256: String,
}

/// The entry of a verified manifest that applies to this board and revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub version: String,
    pub channel: String,
    pub partition_layout: String,
    /// Lowercase hex SHA-256 of the partition table CSV the images were built for.
    pub partition_table_sha256: String,
    pub min_integration_protocol: u64,
    pub app: Artifact,
}

/// Verifies a manifest and selects the entry for the board.
///
/// The signature covers the exact `manifest` bytes; nothing is parsed before it verifies.
///
/// # Errors
/// The [`Reason`] of the first failing step, in the order of the specification.
pub fn verify_manifest(
    manifest: &[u8],
    signature: &[u8],
    keys: &TrustedKeys,
    request: &Request<'_>,
) -> Result<Verified, Reason> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(Reason::Malformed);
    }
    let signature = decode_signature(signature)?;
    let signer = keys
        .keys
        .iter()
        .find(|key| {
            VerifyingKey::from_bytes(key)
                .is_ok_and(|key| key.verify_strict(manifest, &signature).is_ok())
        })
        .ok_or(Reason::BadSignature)?;
    let parsed = parse(manifest)?;
    if parsed.signing_key_id != key_id(signer) {
        return Err(Reason::KeyIdMismatch);
    }
    select(parsed, request)
}

fn decode_signature(text: &[u8]) -> Result<Signature, Reason> {
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    let bytes = STANDARD.decode(text).map_err(|_| Reason::BadSignature)?;
    let bytes: [u8; 64] = bytes.try_into().map_err(|_| Reason::BadSignature)?;
    Ok(Signature::from_bytes(&bytes))
}

/// A parsed, validated manifest.
struct Manifest {
    version: String,
    channel: String,
    signing_key_id: String,
    boards: Vec<BoardEntry>,
}

struct BoardEntry {
    board: String,
    hardware_revisions: Vec<String>,
    partition_layout: String,
    partition_table_sha256: String,
    min_integration_protocol: u64,
    app: Artifact,
}

fn select(manifest: Manifest, request: &Request<'_>) -> Result<Verified, Reason> {
    if request
        .channel
        .is_some_and(|channel| channel != manifest.channel)
    {
        return Err(Reason::ChannelMismatch);
    }
    let entry = manifest
        .boards
        .into_iter()
        .find(|entry| entry.board == request.board)
        .ok_or(Reason::BoardMismatch)?;
    if !entry
        .hardware_revisions
        .iter()
        .any(|revision| revision == request.hardware_revision)
    {
        return Err(Reason::RevisionUnsupported);
    }
    Ok(Verified {
        version: manifest.version,
        channel: manifest.channel,
        partition_layout: entry.partition_layout,
        partition_table_sha256: entry.partition_table_sha256,
        min_integration_protocol: entry.min_integration_protocol,
        app: entry.app,
    })
}

/// JSON that refuses an object with a repeated key.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Walk;
        impl<'de> Visitor<'de> for Walk {
            type Value = Value;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
                Ok(value.into())
            }

            fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
                Ok(value.into())
            }

            fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
                Ok(serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number))
            }

            fn visit_str<E>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }

            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
                let mut items = Vec::new();
                while let Some(Strict(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Value::Array(items))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut object = Map::new();
                while let Some((name, Strict(value))) = map.next_entry::<String, Strict>()? {
                    if object.insert(name, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate key"));
                    }
                }
                Ok(Value::Object(object))
            }
        }
        deserializer.deserialize_any(Walk).map(Strict)
    }
}

fn parse(bytes: &[u8]) -> Result<Manifest, Reason> {
    let Strict(value) = serde_json::from_slice(bytes).map_err(|_| Reason::Malformed)?;
    validate(&value)
}

fn object<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, Reason> {
    let object = value.as_object().ok_or(Reason::Malformed)?;
    let expected: BTreeSet<&str> = keys.iter().copied().collect();
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    (expected == actual)
        .then_some(object)
        .ok_or(Reason::Malformed)
}

fn text(value: &Value, pattern: fn(&str) -> bool) -> Result<&str, Reason> {
    value
        .as_str()
        .filter(|text| pattern(text))
        .ok_or(Reason::Malformed)
}

fn integer(value: &Value, minimum: u64) -> Result<u64, Reason> {
    value
        .as_u64()
        .filter(|number| *number >= minimum)
        .ok_or(Reason::Malformed)
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    text.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

fn is_file_name(text: &str) -> bool {
    let mut chars = text.chars();
    text.len() <= 128
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_sha256(text: &str) -> bool {
    is_hex(text, 64)
}

fn is_commit(text: &str) -> bool {
    is_hex(text, 40)
}

fn is_key_id(text: &str) -> bool {
    is_hex(text, 16)
}

/// `SemVer` 2.0.0 without build metadata.
fn is_semver(text: &str) -> bool {
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    let number = |part: &&str| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (*part == "0" || !part.starts_with('0'))
    };
    numbers.len() == 3
        && numbers.iter().all(number)
        && pre.is_none_or(|pre| {
            pre.split('.')
                .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        })
}

fn is_beta(version: &str) -> bool {
    version
        .rsplit_once("-beta.")
        .is_some_and(|(_, number)| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

fn validate(value: &Value) -> Result<Manifest, Reason> {
    let top = object(
        value,
        &["format", "product", "release", "signing_key_id", "boards"],
    )?;
    if top["format"] != 1 {
        return Err(if top["format"].is_u64() || top["format"].is_i64() {
            Reason::UnsupportedFormat
        } else {
            Reason::Malformed
        });
    }
    if top["product"] != "njorun" {
        return Err(Reason::Malformed);
    }
    let release = object(&top["release"], &["version", "channel", "source_commit"])?;
    let version = text(&release["version"], is_semver)?;
    let channel = release["channel"]
        .as_str()
        .filter(|channel| matches!(*channel, "stable" | "beta"))
        .ok_or(Reason::Malformed)?;
    let channel_fits = if channel == "stable" {
        !version.contains('-')
    } else {
        is_beta(version)
    };
    if !channel_fits {
        return Err(Reason::Malformed);
    }
    text(&release["source_commit"], is_commit)?;
    let signing_key_id = text(&top["signing_key_id"], is_key_id)?;
    let boards = top["boards"]
        .as_array()
        .filter(|boards| !boards.is_empty())
        .ok_or(Reason::Malformed)?;
    let mut names = BTreeSet::new();
    let entries = boards
        .iter()
        .map(|entry| {
            let board = board_entry(entry)?;
            names
                .insert(board.board.clone())
                .then_some(board)
                .ok_or(Reason::Malformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Manifest {
        version: version.to_owned(),
        channel: channel.to_owned(),
        signing_key_id: signing_key_id.to_owned(),
        boards: entries,
    })
}

fn board_entry(value: &Value) -> Result<BoardEntry, Reason> {
    let entry = object(
        value,
        &[
            "board",
            "hardware_revisions",
            "partition_layout",
            "partition_table_sha256",
            "coprocessor",
            "min_integration_protocol",
            "artifacts",
        ],
    )?;
    let board = text(&entry["board"], is_ident)?;
    let revisions = entry["hardware_revisions"]
        .as_array()
        .filter(|revisions| !revisions.is_empty())
        .ok_or(Reason::Malformed)?;
    let hardware_revisions = revisions
        .iter()
        .map(|revision| text(revision, is_ident).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if hardware_revisions.iter().collect::<BTreeSet<_>>().len() != hardware_revisions.len() {
        return Err(Reason::Malformed);
    }
    let layout = text(&entry["partition_layout"], is_ident)?;
    let table = text(&entry["partition_table_sha256"], is_sha256)?;
    let coprocessor = object(&entry["coprocessor"], &["chip", "min_firmware"])?;
    text(&coprocessor["chip"], is_ident)?;
    if !coprocessor["min_firmware"].is_null() {
        text(&coprocessor["min_firmware"], is_semver)?;
    }
    let protocol = integer(&entry["min_integration_protocol"], 1)?;
    let artifacts = object(&entry["artifacts"], &["factory", "app"])?;
    let factory = artifact(&artifacts["factory"])?;
    let app = artifact(&artifacts["app"])?;
    if factory.name == app.name {
        return Err(Reason::Malformed);
    }
    Ok(BoardEntry {
        board: board.to_owned(),
        hardware_revisions,
        partition_layout: layout.to_owned(),
        partition_table_sha256: table.to_owned(),
        min_integration_protocol: protocol,
        app,
    })
}

fn artifact(value: &Value) -> Result<Artifact, Reason> {
    let artifact = object(value, &["name", "size", "sha256"])?;
    Ok(Artifact {
        name: text(&artifact["name"], is_file_name)?.to_owned(),
        size: integer(&artifact["size"], 1)?,
        sha256: text(&artifact["sha256"], is_sha256)?.to_owned(),
    })
}

/// Checks an image while it streams in: size on every chunk, digest at the end.
///
/// The caller switches the boot slot only after [`Self::finish`] returned `Ok`.
#[derive(Clone, Debug)]
pub struct ImageCheck {
    size: u64,
    digest: [u8; 32],
    received: u64,
    hasher: Sha256,
}

impl ImageCheck {
    /// Expects an image of `size` bytes with the lowercase hex SHA-256 `sha256`.
    ///
    /// # Errors
    /// [`Reason::Oversize`] when it does not fit a slot of `slot_bytes`, or
    /// [`Reason::Malformed`] for a digest that is not 64 lowercase hex digits.
    pub fn new(size: u64, sha256: &str, slot_bytes: u64) -> Result<Self, Reason> {
        if !is_sha256(sha256) {
            return Err(Reason::Malformed);
        }
        if size == 0 {
            return Err(Reason::SizeMismatch);
        }
        if size > slot_bytes {
            return Err(Reason::Oversize);
        }
        let mut digest = [0_u8; 32];
        for (index, byte) in digest.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&sha256[index * 2..index * 2 + 2], 16)
                .map_err(|_| Reason::Malformed)?;
        }
        Ok(Self {
            size,
            digest,
            received: 0,
            hasher: Sha256::new(),
        })
    }

    /// Takes the next chunk.
    ///
    /// # Errors
    /// [`Reason::SizeMismatch`] as soon as more than the expected size arrives.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), Reason> {
        let received = self
            .received
            .checked_add(chunk.len() as u64)
            .filter(|received| *received <= self.size)
            .ok_or(Reason::SizeMismatch)?;
        self.received = received;
        self.hasher.update(chunk);
        Ok(())
    }

    /// Bytes taken so far.
    #[must_use]
    pub const fn received(&self) -> u64 {
        self.received
    }

    /// Ends the stream.
    ///
    /// # Errors
    /// [`Reason::SizeMismatch`] for a short image, [`Reason::DigestMismatch`] for another digest.
    pub fn finish(self) -> Result<(), Reason> {
        if self.received != self.size {
            return Err(Reason::SizeMismatch);
        }
        if self.hasher.finalize().as_slice() != self.digest {
            return Err(Reason::DigestMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        ImageCheck, MAX_MANIFEST_BYTES, Reason, Request, TrustedKeys, key_id, verify_manifest,
    };
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use sha2::{Digest, Sha256};

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../protocol/fixtures/firmware-manifest.json"
        ))
        .expect("fixture")
    }

    fn reason(code: &str) -> Option<Reason> {
        [
            Reason::Malformed,
            Reason::BadSignature,
            Reason::UnsupportedFormat,
            Reason::KeyIdMismatch,
            Reason::BoardMismatch,
            Reason::RevisionUnsupported,
            Reason::ChannelMismatch,
            Reason::SizeMismatch,
            Reason::DigestMismatch,
            Reason::Oversize,
        ]
        .into_iter()
        .find(|reason| reason.as_str() == code)
    }

    #[test]
    fn every_fixture_case_gets_the_expected_answer() {
        let fixture = fixture();
        let cases = fixture["cases"].as_array().expect("cases");
        assert!(cases.len() >= 16);
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let texts: Vec<&str> = case["trusted_keys"]
                .as_array()
                .expect("keys")
                .iter()
                .map(|key| key.as_str().expect("key"))
                .collect();
            let keys = TrustedKeys::from_base64(&texts).expect("keys");
            let request = &case["request"];
            let request = Request {
                board: request["board"].as_str().expect("board"),
                hardware_revision: request["hardware_revision"].as_str().expect("revision"),
                channel: request["channel"].as_str(),
            };
            let mut result = verify_manifest(
                case["manifest"].as_str().expect("manifest").as_bytes(),
                case["signature"].as_str().expect("signature").as_bytes(),
                &keys,
                &request,
            )
            .map(|_| ());
            if let (Ok(()), Some(artifact)) = (&result, case.get("artifact")) {
                result = check_artifact(case, artifact);
            }
            let expected = case["expect"].as_str().expect("expect");
            match (expected, result) {
                ("ok", Ok(())) => {}
                (code, Err(reason_found)) => {
                    assert_eq!(Some(reason_found), reason(code), "{name}");
                }
                (code, Ok(())) => panic!("{name}: expected {code}, got ok"),
            }
        }
    }

    /// The artifact cases check bytes against the manifest entry of that kind.
    fn check_artifact(case: &Value, artifact: &Value) -> Result<(), Reason> {
        let data = STANDARD
            .decode(artifact["data"].as_str().expect("data"))
            .expect("base64");
        let manifest: Value =
            serde_json::from_str(case["manifest"].as_str().expect("manifest")).expect("json");
        let kind = artifact["kind"].as_str().expect("kind");
        let entry = &manifest["boards"][0]["artifacts"][kind];
        let size = entry["size"].as_u64().expect("size");
        let slot = case["slot_size"].as_u64().unwrap_or(u64::MAX);
        let slot = if kind == "app" { slot } else { u64::MAX };
        let mut check = ImageCheck::new(size, entry["sha256"].as_str().expect("sha"), slot)?;
        check.update(&data).map_err(|_| Reason::SizeMismatch)?;
        check.finish()
    }

    #[test]
    fn the_embedded_keys_are_the_two_release_keys() {
        let keys = TrustedKeys::embedded().expect("embedded release keys");
        assert_eq!(keys.keys().len(), 2);
        assert_eq!(key_id(&keys.keys()[0]), "7a622ae0a78916f7");
        assert_eq!(key_id(&keys.keys()[1]), "3a352ded7eab9eb8");
    }

    #[test]
    fn damaged_key_files_are_refused() {
        assert_eq!(TrustedKeys::from_release_keys("{}"), Err(Reason::Malformed));
        assert_eq!(
            TrustedKeys::from_base64(&["AAAA"]),
            Err(Reason::Malformed),
            "not 32 bytes"
        );
    }

    #[test]
    fn an_oversized_manifest_is_refused_before_anything_else() {
        let keys = TrustedKeys::embedded().expect("keys");
        let request = Request {
            board: "d1001",
            hardware_revision: "1",
            channel: None,
        };
        let big = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert_eq!(
            verify_manifest(&big, b"x", &keys, &request),
            Err(Reason::Malformed)
        );
    }

    fn digest_of(data: &[u8]) -> String {
        super::hex(&Sha256::digest(data))
    }

    #[test]
    fn a_streamed_image_is_checked_for_size_and_digest() {
        let data: Vec<u8> = (0..=255).cycle().take(10_000).collect();
        let digest = digest_of(&data);
        let mut check = ImageCheck::new(10_000, &digest, 16_384).expect("check");
        for chunk in data.chunks(1_460) {
            check.update(chunk).expect("chunk");
        }
        assert_eq!(check.received(), 10_000);
        check.finish().expect("ok");

        let mut tampered = data.clone();
        tampered[5_000] ^= 1;
        let mut check = ImageCheck::new(10_000, &digest, 16_384).expect("check");
        check.update(&tampered).expect("chunk");
        assert_eq!(check.finish(), Err(Reason::DigestMismatch));
    }

    #[test]
    fn a_streamed_image_that_is_too_long_or_too_short_fails_early_or_late() {
        let data = vec![7_u8; 100];
        let digest = digest_of(&data);
        let mut check = ImageCheck::new(100, &digest, 200).expect("check");
        check.update(&data).expect("all");
        assert_eq!(
            check.update(&[0]),
            Err(Reason::SizeMismatch),
            "one byte over"
        );
        let mut check = ImageCheck::new(100, &digest, 200).expect("check");
        check.update(&data[..99]).expect("short");
        assert_eq!(check.finish(), Err(Reason::SizeMismatch));
    }

    #[test]
    fn the_image_must_fit_the_slot_and_the_digest_must_be_hex() {
        let digest = digest_of(b"x");
        assert!(matches!(
            ImageCheck::new(101, &digest, 100),
            Err(Reason::Oversize)
        ));
        assert!(ImageCheck::new(100, &digest, 100).is_ok(), "inclusive");
        assert!(matches!(
            ImageCheck::new(1, &digest.to_uppercase(), 100),
            Err(Reason::Malformed)
        ));
        assert!(matches!(
            ImageCheck::new(1, "abc", 100),
            Err(Reason::Malformed)
        ));
    }
}
