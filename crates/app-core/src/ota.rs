//! The firmware install itself (decision D7), host side: everything except the HTTP client and
//! the flash driver.
//!
//! [`run`] drives one accepted install through a [`Source`] and a [`Slot`]: it pulls manifest and signature
//! from the HA endpoint, verifies the signature against the built-in keys, compares the manifest
//! with the install command, streams the image into the inactive slot while checking size and
//! digest, and switches the boot slot only when both match. [`PendingInstall`] is what the
//! firmware keeps across the reboot, [`BootValidator`] decides when the new firmware marks itself
//! valid or gives up, and [`capability`] reports `ota` only when the partition table has two OTA
//! slots.
//!
//! See `docs/protocols/002-ha-management.md` and `docs/protocols/003-firmware-manifest.md`.

use njorun_management::firmware::{
    ImageCheck, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES, Reason, Request, TrustedKeys,
    verify_manifest,
};
use serde_json::{Value, json};

use crate::install::{
    Channel, Install, MAX_INSTALL_ID_BYTES, MAX_VERSION_CHARS, Outcome, Stage, outcome_message,
};

/// The board ID this firmware reports and expects in a manifest.
pub const BOARD: &str = "d1001";
/// Minutes the new firmware has to confirm a management session before it rolls back.
pub const CONFIRM_WINDOW_MS: u64 = 5 * 60 * 1_000;
/// Fewest OTA slots for the `ota` capability: the running one and the inactive one.
pub const MIN_OTA_SLOTS: usize = 2;

/// The files HA serves for an install, next to the `url` of the install command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Document {
    Manifest,
    Signature,
    Image,
}

impl Document {
    /// The URL of this document: the install `url` plus a fixed path segment. The token goes
    /// in the `Authorization` header, never in the URL.
    #[must_use]
    pub fn url(self, base: &str) -> String {
        let base = base.trim_end_matches('/');
        let name = match self {
            Self::Manifest => "manifest.json",
            Self::Signature => "manifest.json.sig",
            Self::Image => "app.bin",
        };
        format!("{base}/{name}")
    }
}

/// Why an install failed before the reboot. None of them touches the running firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The endpoint could not be reached, refused the token or broke off.
    Transport,
    /// The manifest is not the signed release of this board, or its image is not as signed.
    Manifest(Reason),
    /// The manifest and the install command disagree on the named item.
    Mismatch(&'static str),
    /// The inactive slot could not be written or does not hold a valid image.
    Slot,
    /// The new firmware's pending record could not be stored, so the slot is not switched.
    Persist,
}

impl Failure {
    /// A short code for the log. Never contains the token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Manifest(reason) => reason.as_str(),
            Self::Mismatch(what) => what,
            Self::Slot => "slot",
            Self::Persist => "persist",
        }
    }
}

/// The HA endpoint of one install, with the install's token. Everything it fetches is
/// untrusted until [`run`] has verified it.
pub trait Source {
    /// Fetches a small document, at most `limit` bytes.
    ///
    /// # Errors
    /// [`Failure::Transport`] for a failed or refused request or a body over the limit.
    fn fetch(&mut self, document: Document, limit: usize) -> Result<Vec<u8>, Failure>;

    /// Streams the image into `sink`, chunk by chunk, until the body ends.
    ///
    /// # Errors
    /// [`Failure::Transport`], or the failure `sink` returned, which ends the stream.
    fn stream(&mut self, sink: &mut dyn FnMut(&[u8]) -> Result<(), Failure>)
    -> Result<(), Failure>;
}

/// The inactive OTA slot and the record the firmware keeps across the reboot.
pub trait Slot {
    /// Opens the slot for an image of `size` bytes.
    ///
    /// # Errors
    /// [`Failure::Slot`].
    fn begin(&mut self, size: u32) -> Result<(), Failure>;

    /// Appends to the slot.
    ///
    /// # Errors
    /// [`Failure::Slot`].
    fn write(&mut self, chunk: &[u8]) -> Result<(), Failure>;

    /// Closes the slot and lets the platform validate the image it holds. Does not switch.
    ///
    /// # Errors
    /// [`Failure::Slot`] when the image is not a valid application image.
    fn finish(&mut self) -> Result<(), Failure>;

    /// Gives up on the slot after a failure. The boot slot is unchanged.
    fn abort(&mut self);

    /// Stores what is needed to report the outcome after the reboot. Called after the image
    /// was fully verified and before the slot is switched.
    ///
    /// # Errors
    /// [`Failure::Persist`].
    fn persist(&mut self) -> Result<(), Failure>;

    /// Makes the written slot the next boot slot.
    ///
    /// # Errors
    /// [`Failure::Slot`].
    fn activate(&mut self) -> Result<(), Failure>;
}

/// What the tablet knows about itself when it checks a release.
#[derive(Clone, Copy, Debug)]
pub struct Hardware<'a> {
    pub board: &'a str,
    pub hardware_revision: &'a str,
    /// Lowercase hex SHA-256 of the partition table CSV this firmware was built with. The
    /// manifest binds its images to a table, and a different one needs a USB reflash.
    pub partition_table_sha256: &'a str,
    /// Size of the inactive slot in bytes.
    pub slot_bytes: u32,
}

/// Runs an accepted install up to, and including, the switch of the boot slot.
///
/// Stages passed to `progress`: `download` while the image streams into the slot (download
/// and write are one pass, `done` counts bytes written), then `verify` for the final digest
/// check and the image validation. The caller reports `reboot` and restarts after `Ok`.
///
/// The image is switched to only after the signature, the comparison with the install command,
/// the size and the digest all passed; on any failure the slot is aborted, the boot slot is
/// unchanged and nothing was persisted.
///
/// # Errors
/// The [`Failure`] that stopped the install.
pub fn run(
    install: &Install,
    hardware: &Hardware<'_>,
    keys: &TrustedKeys,
    source: &mut dyn Source,
    slot: &mut dyn Slot,
    progress: &mut dyn FnMut(Stage, u32, u32),
) -> Result<(), Failure> {
    let total = install.size;
    progress(Stage::Download, 0, total);
    let manifest = source.fetch(Document::Manifest, MAX_MANIFEST_BYTES)?;
    let signature = source.fetch(Document::Signature, MAX_SIGNATURE_BYTES)?;
    let request = Request {
        board: hardware.board,
        hardware_revision: hardware.hardware_revision,
        channel: Some(match install.channel {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }),
    };
    let release =
        verify_manifest(&manifest, &signature, keys, &request).map_err(Failure::Manifest)?;
    if release.version != install.version {
        return Err(Failure::Mismatch("version"));
    }
    if release.app.sha256 != install.sha256 {
        return Err(Failure::Mismatch("digest"));
    }
    if release.app.size != u64::from(install.size) {
        return Err(Failure::Mismatch("size"));
    }
    if release.partition_table_sha256 != hardware.partition_table_sha256 {
        return Err(Failure::Mismatch("partition_layout"));
    }
    let mut check = ImageCheck::new(
        u64::from(install.size),
        &install.sha256,
        u64::from(hardware.slot_bytes),
    )
    .map_err(Failure::Manifest)?;
    slot.begin(install.size)?;
    let mut done = 0_u32;
    let streamed = source.stream(&mut |chunk| {
        check.update(chunk).map_err(Failure::Manifest)?;
        slot.write(chunk)?;
        done = u32::try_from(check.received()).unwrap_or(u32::MAX);
        progress(Stage::Download, done, total);
        Ok(())
    });
    let verified = streamed.and_then(|()| {
        progress(Stage::Verify, 0, total);
        check.finish().map_err(Failure::Manifest)?;
        slot.finish()?;
        progress(Stage::Verify, total, total);
        Ok(())
    });
    if let Err(failure) = verified {
        slot.abort();
        return Err(failure);
    }
    // After the slot was verified and before it becomes the boot slot, so a reboot always finds
    // the record of an install that switched.
    slot.persist().inspect_err(|_| slot.abort())?;
    slot.activate().inspect_err(|_| slot.abort())
}

/// The origin (`scheme://host:port`) of the Home Assistant base URL. An install must download
/// from it, so the token never goes to another host. `None` for a URL without scheme or host.
#[must_use]
pub fn origin_of(base_url: &str) -> Option<String> {
    let (scheme, rest) = base_url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|a| !a.is_empty())?;
    Some(format!("{scheme}://{authority}"))
}

/// What the firmware keeps across the reboot of an install, to report its outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInstall {
    pub install_id: String,
    /// The version the install asked for.
    pub version: String,
}

impl PendingInstall {
    /// The stored text.
    #[must_use]
    pub fn encode(&self) -> String {
        json!({"install_id": self.install_id, "version": self.version}).to_string()
    }

    /// Reads the stored text; `None` when it is not what [`Self::encode`] writes.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let object = value.as_object().filter(|object| object.len() == 2)?;
        let install_id = object.get("install_id")?.as_str()?;
        let version = object.get("version")?.as_str()?;
        let plain = |text: &str, limit: usize| {
            (1..=limit).contains(&text.chars().count()) && !text.chars().any(char::is_control)
        };
        (plain(install_id, MAX_INSTALL_ID_BYTES) && plain(version, MAX_VERSION_CHARS)).then(|| {
            Self {
                install_id: install_id.to_owned(),
                version: version.to_owned(),
            }
        })
    }

    /// The one `install_outcome` to send on the first session after the boot, and the outcome
    /// it carries. `running` is the version that runs now and `confirmed` whether this boot
    /// was confirmed by the health check.
    #[must_use]
    pub fn outcome(&self, running: &str, confirmed: bool) -> Option<(Outcome, Value)> {
        let outcome = Outcome::reconcile(&self.version, running, confirmed);
        outcome_message(&self.install_id, outcome, running).map(|message| (outcome, message))
    }
}

/// What the health check does next for a firmware that boots as a new install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing: not a new install, already settled, or still waiting for the session.
    Wait,
    /// Mark the running firmware valid, now.
    MarkValid,
    /// No confirmed session in time: reboot into the previous firmware.
    Rollback,
}

/// Decides when a new firmware marks itself valid.
///
/// A firmware that the bootloader started for the first time after an install waits for a
/// confirmed management session. It then marks itself valid. Without one within
/// [`CONFIRM_WINDOW_MS`] of the boot it asks the bootloader to roll back, so a firmware that
/// cannot reach Home Assistant never stays. A firmware that was not installed through the
/// install protocol (flashed over USB, so no pending record) is not held to this.
#[derive(Clone, Copy, Debug)]
pub struct BootValidator {
    waiting: bool,
    booted_ms: u64,
}

impl BootValidator {
    /// `awaiting_verdict`: the bootloader reports the running image as pending verification.
    /// `has_record`: a [`PendingInstall`] is stored. `now_ms` is the monotonic time at boot.
    #[must_use]
    pub const fn new(awaiting_verdict: bool, has_record: bool, now_ms: u64) -> Self {
        Self {
            waiting: awaiting_verdict && has_record,
            booted_ms: now_ms,
        }
    }

    /// Whether this boot waits for a verdict.
    #[must_use]
    pub const fn waiting(&self) -> bool {
        self.waiting
    }

    /// The next step. `session_confirmed`: the management session is established and Home
    /// Assistant answered. Returns [`Step::MarkValid`] or [`Step::Rollback`] once and settles.
    pub const fn poll(&mut self, now_ms: u64, session_confirmed: bool) -> Step {
        if !self.waiting {
            return Step::Wait;
        }
        if session_confirmed {
            self.waiting = false;
            return Step::MarkValid;
        }
        if now_ms.saturating_sub(self.booted_ms) >= CONFIRM_WINDOW_MS {
            self.waiting = false;
            return Step::Rollback;
        }
        Step::Wait
    }

    /// Marking valid failed; keep waiting so the next poll tries again until the window ends.
    pub const fn mark_failed(&mut self) {
        self.waiting = true;
    }
}

/// The `ota` capability: the board ID, only when the partition table has two OTA slots. A board
/// without them (or a table that was not read) reports nothing and updates through the
/// installer page.
#[must_use]
pub fn capability(board: &str, ota_slots: usize) -> Option<Value> {
    (ota_slots >= MIN_OTA_SLOTS).then(|| json!({"board": board}))
}

#[cfg(test)]
mod tests {
    use njorun_management::firmware::{Reason, TrustedKeys};
    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::{
        BOARD, BootValidator, CONFIRM_WINDOW_MS, Document, Failure, Hardware, PendingInstall, Slot,
        Source, Step, capability, origin_of, run,
    };
    use crate::install::{Channel, Install, Outcome, Stage};

    // A manifest signed by the fixture's throwaway key, with its image.
    struct Release {
        manifest: Vec<u8>,
        signature: Vec<u8>,
        image: Vec<u8>,
        keys: TrustedKeys,
        install: Install,
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
    }

    /// The fixture case `artifact_matches` describes a 64-byte app image.
    fn release() -> Release {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/firmware-manifest.json"
        ))
        .expect("fixture");
        let case = fixture["cases"]
            .as_array()
            .expect("cases")
            .iter()
            .find(|case| case["name"] == "artifact_matches")
            .expect("case");
        let image = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(case["artifact"]["data"].as_str().expect("data"))
                .expect("image")
        };
        let manifest = case["manifest"].as_str().expect("manifest");
        let parsed: serde_json::Value = serde_json::from_str(manifest).expect("json");
        let texts: Vec<&str> = case["trusted_keys"]
            .as_array()
            .expect("keys")
            .iter()
            .map(|key| key.as_str().expect("key"))
            .collect();
        Release {
            manifest: manifest.as_bytes().to_vec(),
            signature: case["signature"]
                .as_str()
                .expect("signature")
                .as_bytes()
                .to_vec(),
            keys: TrustedKeys::from_base64(&texts).expect("keys"),
            install: Install {
                install_id: "i1".into(),
                version: parsed["release"]["version"].as_str().expect("v").into(),
                channel: Channel::Beta,
                size: u32::try_from(image.len()).expect("size"),
                sha256: hex(&Sha256::digest(&image)),
                url: "https://ha.local:8123/api/njorun/firmware/x".into(),
                token: "T".repeat(43),
                expires_at: 1,
                downgrade: false,
            },
            image,
        }
    }

    fn table_digest() -> String {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/firmware-manifest.json"
        ))
        .expect("fixture");
        let manifest: serde_json::Value =
            serde_json::from_str(fixture["cases"][0]["manifest"].as_str().expect("manifest"))
                .expect("json");
        manifest["boards"][0]["partition_table_sha256"]
            .as_str()
            .expect("sha")
            .to_owned()
    }

    #[derive(Default)]
    struct Fake {
        manifest: Vec<u8>,
        signature: Vec<u8>,
        image: Vec<u8>,
        chunk: usize,
        fail_fetch: bool,
        fail_stream_after: Option<usize>,
        fetched: Vec<Document>,
    }

    impl Source for Fake {
        fn fetch(&mut self, document: Document, _limit: usize) -> Result<Vec<u8>, Failure> {
            self.fetched.push(document);
            if self.fail_fetch {
                return Err(Failure::Transport);
            }
            Ok(match document {
                Document::Manifest => self.manifest.clone(),
                Document::Signature => self.signature.clone(),
                Document::Image => unreachable!("streamed"),
            })
        }

        fn stream(
            &mut self,
            sink: &mut dyn FnMut(&[u8]) -> Result<(), Failure>,
        ) -> Result<(), Failure> {
            self.fetched.push(Document::Image);
            for (index, chunk) in self.image.chunks(self.chunk.max(1)).enumerate() {
                if self.fail_stream_after == Some(index) {
                    return Err(Failure::Transport);
                }
                sink(chunk)?;
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Flash {
        written: Vec<u8>,
        events: Vec<&'static str>,
        fail_finish: bool,
        fail_persist: bool,
        fail_write: bool,
    }

    impl Slot for Flash {
        fn begin(&mut self, _size: u32) -> Result<(), Failure> {
            self.events.push("begin");
            Ok(())
        }

        fn write(&mut self, chunk: &[u8]) -> Result<(), Failure> {
            if self.fail_write {
                return Err(Failure::Slot);
            }
            self.written.extend_from_slice(chunk);
            Ok(())
        }

        fn finish(&mut self) -> Result<(), Failure> {
            self.events.push("finish");
            if self.fail_finish {
                Err(Failure::Slot)
            } else {
                Ok(())
            }
        }

        fn abort(&mut self) {
            self.events.push("abort");
        }

        fn persist(&mut self) -> Result<(), Failure> {
            self.events.push("persist");
            if self.fail_persist {
                Err(Failure::Persist)
            } else {
                Ok(())
            }
        }

        fn activate(&mut self) -> Result<(), Failure> {
            self.events.push("activate");
            Ok(())
        }
    }

    fn hardware(table: &str, slot_bytes: u32) -> Hardware<'_> {
        Hardware {
            board: BOARD,
            hardware_revision: "1",
            partition_table_sha256: table,
            slot_bytes,
        }
    }

    fn source(release: &Release, chunk: usize) -> Fake {
        Fake {
            manifest: release.manifest.clone(),
            signature: release.signature.clone(),
            image: release.image.clone(),
            chunk,
            ..Fake::default()
        }
    }

    type Progress = Vec<(Stage, u32, u32)>;

    fn go(
        release: &Release,
        source: &mut Fake,
        flash: &mut Flash,
        slot_bytes: u32,
    ) -> (Result<(), Failure>, Progress) {
        let table = table_digest();
        let mut seen = Vec::new();
        let result = run(
            &release.install,
            &hardware(&table, slot_bytes),
            &release.keys,
            source,
            flash,
            &mut |stage, done, total| seen.push((stage, done, total)),
        );
        (result, seen)
    }

    #[test]
    fn a_verified_image_is_streamed_and_the_slot_switched_last() {
        let release = release();
        let mut source = source(&release, 16);
        let mut flash = Flash::default();
        let (result, progress) = go(&release, &mut source, &mut flash, 1 << 20);
        assert_eq!(result, Ok(()));
        assert_eq!(flash.written, release.image);
        assert_eq!(flash.events, ["begin", "finish", "persist", "activate"]);
        assert_eq!(
            source.fetched,
            [Document::Manifest, Document::Signature, Document::Image]
        );
        let total = release.install.size;
        assert_eq!(progress.first(), Some(&(Stage::Download, 0, total)));
        assert_eq!(progress.last(), Some(&(Stage::Verify, total, total)));
        assert!(progress.contains(&(Stage::Download, 16, total)));
        assert!(
            progress.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "never backwards"
        );
    }

    #[test]
    fn a_manifest_signed_by_another_key_is_refused_before_any_image_byte() {
        let release = release();
        let mut source = source(&release, 16);
        let mut flash = Flash::default();
        let other = TrustedKeys::from_base64(&["uFoq5wq5Nv7t6GPU3sFdMI+3g7ec3YQ0dAmN4Csy5ss="])
            .expect("key");
        let table = table_digest();
        let result = run(
            &release.install,
            &hardware(&table, 1 << 20),
            &other,
            &mut source,
            &mut flash,
            &mut |_, _, _| {},
        );
        assert_eq!(result, Err(Failure::Manifest(Reason::BadSignature)));
        assert_eq!(flash.events, [] as [&str; 0], "the slot was never opened");
        assert!(!source.fetched.contains(&Document::Image));
    }

    #[test]
    fn a_tampered_manifest_is_a_bad_signature() {
        let release = release();
        let mut source = source(&release, 16);
        source.manifest[10] ^= 1;
        let mut flash = Flash::default();
        let (result, _) = go(&release, &mut source, &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Manifest(Reason::BadSignature)));
        assert_eq!(flash.events, [] as [&str; 0]);
    }

    #[test]
    fn the_command_must_agree_with_the_signed_manifest() {
        let base = release();
        for (name, change) in [
            ("version", "version"),
            ("digest", "digest"),
            ("size", "size"),
        ] {
            let mut release = release();
            match change {
                "version" => release.install.version = "9.9.9".into(),
                "digest" => release.install.sha256 = "0".repeat(64),
                _ => release.install.size += 1,
            }
            let mut source = source(&base, 16);
            let mut flash = Flash::default();
            let (result, _) = go(&release, &mut source, &mut flash, 1 << 20);
            assert_eq!(result, Err(Failure::Mismatch(name)), "{name}");
            assert_eq!(flash.events, [] as [&str; 0], "{name}: nothing was written");
        }
    }

    #[test]
    fn another_channel_board_or_partition_table_is_refused() {
        let mut stable = release();
        stable.install.channel = Channel::Stable;
        let mut flash = Flash::default();
        let (result, _) = go(&stable, &mut source(&stable, 16), &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Manifest(Reason::ChannelMismatch)));

        let release = release();
        let mut flash = Flash::default();
        let wrong = "f".repeat(64);
        let result = run(
            &release.install,
            &hardware(&wrong, 1 << 20),
            &release.keys,
            &mut source(&release, 16),
            &mut flash,
            &mut |_, _, _| {},
        );
        assert_eq!(result, Err(Failure::Mismatch("partition_layout")));

        let table = table_digest();
        let other_board = Hardware {
            board: "d2000",
            ..hardware(&table, 1 << 20)
        };
        let result = run(
            &release.install,
            &other_board,
            &release.keys,
            &mut source(&release, 16),
            &mut Flash::default(),
            &mut |_, _, _| {},
        );
        assert_eq!(result, Err(Failure::Manifest(Reason::BoardMismatch)));
    }

    #[test]
    fn an_image_that_does_not_fit_the_slot_is_refused_without_opening_it() {
        let release = release();
        let mut flash = Flash::default();
        let (result, _) = go(&release, &mut source(&release, 16), &mut flash, 63);
        assert_eq!(result, Err(Failure::Manifest(Reason::Oversize)));
        assert_eq!(flash.events, [] as [&str; 0]);
    }

    #[test]
    fn a_corrupted_image_is_never_activated() {
        let release = release();
        let mut tampered = source(&release, 16);
        tampered.image[3] ^= 1;
        let mut flash = Flash::default();
        let (result, progress) = go(&release, &mut tampered, &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Manifest(Reason::DigestMismatch)));
        assert_eq!(flash.events, ["begin", "abort"]);
        assert_eq!(progress.last().map(|entry| entry.0), Some(Stage::Verify));
    }

    #[test]
    fn a_longer_or_shorter_image_is_caught_by_size() {
        let release = release();
        let mut long = source(&release, 16);
        long.image.push(0);
        let mut flash = Flash::default();
        let (result, _) = go(&release, &mut long, &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Manifest(Reason::SizeMismatch)));
        assert_eq!(flash.events, ["begin", "abort"]);

        let mut short = source(&release, 16);
        short.image.pop();
        let mut flash = Flash::default();
        let (result, _) = go(&release, &mut short, &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Manifest(Reason::SizeMismatch)));
        assert_eq!(flash.events, ["begin", "abort"]);
    }

    #[test]
    fn transport_slot_and_persist_failures_abort_and_never_activate() {
        let release = release();

        let mut flash = Flash::default();
        let mut broken = source(&release, 16);
        broken.fail_fetch = true;
        let (result, _) = go(&release, &mut broken, &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Transport));
        assert_eq!(flash.events, [] as [&str; 0]);

        let mut flash = Flash::default();
        let mut cut = source(&release, 16);
        cut.fail_stream_after = Some(2);
        let (result, _) = go(&release, &mut cut, &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Transport));
        assert_eq!(flash.events, ["begin", "abort"]);

        let mut flash = Flash {
            fail_write: true,
            ..Flash::default()
        };
        let (result, _) = go(&release, &mut source(&release, 16), &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Slot));
        assert_eq!(flash.events, ["begin", "abort"]);

        let mut flash = Flash {
            fail_finish: true,
            ..Flash::default()
        };
        let (result, _) = go(&release, &mut source(&release, 16), &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Slot));
        assert_eq!(flash.events, ["begin", "finish", "abort"]);

        let mut flash = Flash {
            fail_persist: true,
            ..Flash::default()
        };
        let (result, _) = go(&release, &mut source(&release, 16), &mut flash, 1 << 20);
        assert_eq!(result, Err(Failure::Persist));
        assert_eq!(flash.events, ["begin", "finish", "persist", "abort"]);
    }

    #[test]
    fn the_origin_is_scheme_and_authority_of_the_ha_url() {
        assert_eq!(
            origin_of("https://ha.local:8123"),
            Some("https://ha.local:8123".into())
        );
        assert_eq!(
            origin_of("http://192.168.1.5:8123/some/path/"),
            Some("http://192.168.1.5:8123".into())
        );
        assert_eq!(origin_of("ha.local"), None);
        assert_eq!(origin_of("ftp://ha.local"), None);
        assert_eq!(origin_of("https:///x"), None);
    }

    #[test]
    fn documents_are_next_to_the_install_url() {
        let base = "https://ha.local:8123/api/njorun/firmware/abc";
        assert_eq!(
            Document::Manifest.url(base),
            format!("{base}/manifest.json")
        );
        assert_eq!(
            Document::Signature.url(&format!("{base}/")),
            format!("{base}/manifest.json.sig")
        );
        assert_eq!(Document::Image.url(base), format!("{base}/app.bin"));
    }

    #[test]
    fn the_pending_record_round_trips_and_rejects_junk() {
        let pending = PendingInstall {
            install_id: "i1".into(),
            version: "0.2.0".into(),
        };
        assert_eq!(PendingInstall::decode(&pending.encode()), Some(pending));
        for junk in [
            "",
            "{}",
            "[]",
            r#"{"install_id":"i1"}"#,
            r#"{"install_id":"","version":"1.0.0"}"#,
            r#"{"install_id":"i1","version":"1.0.0","extra":1}"#,
            r#"{"install_id":7,"version":"1.0.0"}"#,
        ] {
            assert_eq!(PendingInstall::decode(junk), None, "{junk}");
        }
    }

    #[test]
    fn the_outcome_follows_the_running_version_and_the_confirmed_boot() {
        let pending = PendingInstall {
            install_id: "i1".into(),
            version: "0.2.0".into(),
        };
        let (outcome, message) = pending.outcome("0.2.0", true).expect("outcome");
        assert_eq!(outcome, Outcome::Installed);
        assert_eq!(message["state"], "installed");
        assert_eq!(message["version"], "0.2.0");
        let (outcome, message) = pending.outcome("0.1.0", false).expect("outcome");
        assert_eq!(outcome, Outcome::RolledBack);
        assert_eq!(message["version"], "0.1.0");
        let (outcome, _) = pending.outcome("0.2.0", false).expect("outcome");
        assert_eq!(
            outcome,
            Outcome::RolledBack,
            "an unconfirmed boot is no install"
        );
    }

    #[test]
    fn a_new_install_is_valid_only_after_a_confirmed_session() {
        let mut validator = BootValidator::new(true, true, 1_000);
        assert!(validator.waiting());
        assert_eq!(validator.poll(2_000, false), Step::Wait);
        assert_eq!(validator.poll(60_000, false), Step::Wait);
        assert_eq!(validator.poll(61_000, true), Step::MarkValid);
        assert!(!validator.waiting());
        assert_eq!(
            validator.poll(CONFIRM_WINDOW_MS * 3, false),
            Step::Wait,
            "settled"
        );
    }

    #[test]
    fn without_a_session_the_bootloader_is_asked_to_roll_back_after_five_minutes() {
        let mut validator = BootValidator::new(true, true, 10_000);
        assert_eq!(
            validator.poll(10_000 + CONFIRM_WINDOW_MS - 1, false),
            Step::Wait
        );
        assert_eq!(
            validator.poll(10_000 + CONFIRM_WINDOW_MS, false),
            Step::Rollback
        );
        assert_eq!(
            validator.poll(10_000 + CONFIRM_WINDOW_MS + 1, false),
            Step::Wait
        );
    }

    #[test]
    fn a_confirmation_that_failed_to_mark_is_tried_again_until_the_window_ends() {
        let mut validator = BootValidator::new(true, true, 0);
        assert_eq!(validator.poll(1_000, true), Step::MarkValid);
        validator.mark_failed();
        assert_eq!(validator.poll(2_000, true), Step::MarkValid);
        validator.mark_failed();
        assert_eq!(validator.poll(CONFIRM_WINDOW_MS, false), Step::Rollback);
    }

    #[test]
    fn only_a_pending_verify_boot_with_a_record_is_held_to_the_window() {
        for (awaiting, record) in [(false, true), (true, false), (false, false)] {
            let mut validator = BootValidator::new(awaiting, record, 0);
            assert!(!validator.waiting());
            assert_eq!(validator.poll(CONFIRM_WINDOW_MS * 2, false), Step::Wait);
            assert_eq!(validator.poll(CONFIRM_WINDOW_MS * 2, true), Step::Wait);
        }
    }

    #[test]
    fn ota_is_reported_only_with_two_ota_slots() {
        assert_eq!(capability(BOARD, 2), Some(json!({"board": "d1001"})));
        assert_eq!(capability(BOARD, 3), Some(json!({"board": "d1001"})));
        assert_eq!(capability(BOARD, 1), None);
        assert_eq!(capability(BOARD, 0), None);
    }
}
