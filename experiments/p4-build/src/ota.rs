//! The firmware install on the device: the HTTP stream from the HA endpoint and the inactive
//! OTA slot behind the host-tested flow in `njorun_app_core::ota`.
//!
//! Runs on the Home Assistant worker thread while an install is in progress; see
//! `management.rs`.

use njorun_app_core::ota::{self, Document, Failure, Hardware, PendingInstall, Slot, Source};
use njorun_management::firmware::TrustedKeys;
use sha2::{Digest, Sha256};

use crate::{board, store};

/// The version this firmware runs, as the manifest names it. A release build sets
/// `NJORUN_VERSION`; otherwise it is the crate version.
pub(crate) const VERSION: &str = match option_env!("NJORUN_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
/// The hardware revision the D1001 reports to the manifest check.
const HARDWARE_REVISION: &str = "1";
/// The partition table this firmware was built for; a manifest binds its images to its digest.
const PARTITION_TABLE: &[u8] = include_bytes!("../partitions.d1001.csv");
const CHUNK_BYTES: usize = 16 * 1024;

/// What the install needs to know about this device, read once per install.
pub(crate) struct Device {
    pub keys: TrustedKeys,
    pub slot_bytes: u32,
    table_sha256: String,
}

impl Device {
    /// `None` when the install cannot work here: the partition table has no two OTA slots or
    /// the built-in keys are damaged.
    pub(crate) fn probe() -> Option<Self> {
        let (slots, slot_bytes) = board::ota_partitions().ok()?;
        ota::capability(ota::BOARD, slots)?;
        let keys = TrustedKeys::embedded()
            .map_err(
                |reason| log::error!(target: "ota", "built-in release keys unusable: {reason}"),
            )
            .ok()?;
        let table_sha256 = Sha256::digest(PARTITION_TABLE)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some(Self {
            keys,
            slot_bytes,
            table_sha256,
        })
    }

    pub(crate) fn hardware(&self) -> Hardware<'_> {
        Hardware {
            board: ota::BOARD,
            hardware_revision: HARDWARE_REVISION,
            partition_table_sha256: &self.table_sha256,
            slot_bytes: self.slot_bytes,
        }
    }
}

/// The HA endpoint of one install. The token goes only into the `Authorization` header.
pub(crate) struct HaSource {
    pub base_url: String,
    pub token: String,
}

fn transport(code: i32) -> Failure {
    log::warn!(target: "ota", "download failed: error {code}");
    Failure::Transport
}

/// Closes the HTTP stream on every way out.
struct Open;

impl Open {
    fn new(url: &str, token: &str) -> Result<(Self, u32), Failure> {
        let length = board::ota_http_open(url, token).map_err(transport)?;
        Ok((Self, length))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        board::ota_http_close();
    }
}

impl Source for HaSource {
    fn fetch(&mut self, document: Document, limit: usize) -> Result<Vec<u8>, Failure> {
        let (_open, length) = Open::new(&document.url(&self.base_url), &self.token)?;
        if usize::try_from(length).map_or(true, |length| length > limit) {
            return Err(Failure::Transport);
        }
        let mut body = Vec::with_capacity(limit.min(4096));
        let mut buffer = vec![0_u8; 1024];
        loop {
            let read = board::ota_http_read(&mut buffer).map_err(transport)?;
            if read == 0 {
                return Ok(body);
            }
            if body.len() + read > limit {
                return Err(Failure::Transport);
            }
            body.extend_from_slice(&buffer[..read]);
        }
    }

    fn stream(
        &mut self,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let (_open, _length) = Open::new(&Document::Image.url(&self.base_url), &self.token)?;
        let mut buffer = vec![0_u8; CHUNK_BYTES];
        loop {
            let read = board::ota_http_read(&mut buffer).map_err(transport)?;
            if read == 0 {
                return Ok(());
            }
            sink(&buffer[..read])?;
        }
    }
}

/// The inactive slot and the record that survives the reboot.
pub(crate) struct FlashSlot {
    pub pending: PendingInstall,
    persisted: bool,
}

impl FlashSlot {
    pub(crate) const fn new(pending: PendingInstall) -> Self {
        Self {
            pending,
            persisted: false,
        }
    }
}

fn slot_error(what: &str, code: i32) -> Failure {
    log::warn!(target: "ota", "slot {what} failed: error {code}");
    Failure::Slot
}

impl Slot for FlashSlot {
    fn begin(&mut self, size: u32) -> Result<(), Failure> {
        board::ota_begin(size).map_err(|code| slot_error("begin", code))
    }

    fn write(&mut self, chunk: &[u8]) -> Result<(), Failure> {
        board::ota_write(chunk).map_err(|code| slot_error("write", code))
    }

    fn finish(&mut self) -> Result<(), Failure> {
        board::ota_end().map_err(|code| slot_error("validation", code))
    }

    fn abort(&mut self) {
        board::ota_abort();
        if self.persisted {
            store::clear_pending_install();
            self.persisted = false;
        }
    }

    fn persist(&mut self) -> Result<(), Failure> {
        if !store::save_pending_install(&self.pending) {
            return Err(Failure::Persist);
        }
        self.persisted = true;
        Ok(())
    }

    fn activate(&mut self) -> Result<(), Failure> {
        board::ota_activate().map_err(|code| slot_error("activation", code))
    }
}
