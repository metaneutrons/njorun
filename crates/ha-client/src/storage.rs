//! Atomic private host-state files. This is not an NVS adapter.

use crate::ClientError;
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

/// Read a bounded JSON state file without including its data in errors.
///
/// # Errors
/// Returns `Storage` for inaccessible, oversized or malformed state.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, ClientError> {
    read_bounded(path, 65536)
}

/// Read credentials from a regular file with no Unix group/world permissions.
///
/// # Errors
/// Returns `Storage` for unsafe permissions or inaccessible/invalid credentials.
pub fn read_private<T: DeserializeOwned>(path: &Path) -> Result<T, ClientError> {
    read_file(path, 65536, true)
}

/// Read larger public dashboard caches with an explicit memory bound.
///
/// # Errors
/// Returns `Storage` for invalid limits, oversized files or invalid JSON.
pub fn read_bounded<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T, ClientError> {
    read_file(path, limit, false)
}

fn read_file<T: DeserializeOwned>(
    path: &Path,
    limit: usize,
    private: bool,
) -> Result<T, ClientError> {
    if limit == 0 || limit > 8 * 1024 * 1024 {
        return Err(ClientError::Storage);
    }
    let file = fs::File::open(path).map_err(|_| ClientError::Storage)?;
    if private {
        let metadata = file.metadata().map_err(|_| ClientError::Storage)?;
        if !metadata.is_file() {
            return Err(ClientError::Storage);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(ClientError::Storage);
            }
        }
    }
    let mut data = Vec::new();
    file.take(u64::try_from(limit).map_err(|_| ClientError::Storage)? + 1)
        .read_to_end(&mut data)
        .map_err(|_| ClientError::Storage)?;
    if data.len() > limit {
        return Err(ClientError::Storage);
    }
    serde_json::from_slice(&data).map_err(|_| ClientError::Storage)
}

/// Replace a private state file only after syncing the complete candidate.
///
/// # Errors
/// Returns `Storage` before replacement, or `StorageCommitUncertain` when the
/// file was replaced but syncing its parent directory failed.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), ClientError> {
    write_bounded(path, value, 65536)
}

/// Atomically write a public dashboard cache with a caller-selected size bound.
///
/// # Errors
/// Returns `Storage` before replacement, or `StorageCommitUncertain` afterward.
pub fn write_bounded<T: Serialize>(
    path: &Path,
    value: &T,
    limit: usize,
) -> Result<(), ClientError> {
    write_with_directory_sync(path, value, limit, |parent| {
        fs::File::open(parent)?.sync_all()
    })
}

fn write_with_directory_sync<T: Serialize>(
    path: &Path,
    value: &T,
    limit: usize,
    sync_directory: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), ClientError> {
    if limit == 0 || limit > 8 * 1024 * 1024 {
        return Err(ClientError::Storage);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|_| ClientError::Storage)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ClientError::Storage)?
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let data = serde_json::to_vec(value).map_err(|_| ClientError::Storage)?;
    if data.len() > limit {
        return Err(ClientError::Storage);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|_| ClientError::Storage)?;
    let result = (|| {
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_: std::io::Error| ClientError::Storage)?;
    sync_directory(parent).map_err(|_| ClientError::StorageCommitUncertain)
}

#[cfg(test)]
#[path = "../tests/support/storage_tests.rs"]
mod tests;
