//! Exercise the real CLI's identity and startup failure boundaries.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "njorun-cli-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("private fixture directory");
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_njorun-simulator"))
        .args(args)
        .output()
        .expect("simulator process")
}

#[test]
fn identity_is_private_and_existing_trust_is_never_replaced() {
    let dir = Directory::new();
    let path = dir.0.join("identity.json");
    let path_arg = path.to_str().expect("fixture path");
    let first = run(&["identity", path_arg]);
    assert!(first.status.success());
    let bytes = fs::read(&path).expect("identity file");
    let identity: serde_json::Value = serde_json::from_slice(&bytes).expect("identity JSON");
    assert_eq!(identity.as_object().expect("object").len(), 2);
    for key in ["private_key", "public_key"] {
        let value = identity[key].as_str().expect("base64 key");
        assert_eq!(value.len(), 44);
        assert!(value.ends_with('='));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }
    let second = run(&["identity", path_arg]);
    assert!(!second.status.success());
    assert_eq!(fs::read(&path).expect("unchanged identity"), bytes);
    assert_eq!(second.stdout.len(), 0);
    assert!(String::from_utf8_lossy(&second.stderr).contains("refusing to replace trust"));
}

#[test]
fn invalid_startup_arguments_fail_before_starting_a_window() {
    for args in [
        vec![],
        vec!["unknown"],
        vec!["run"],
        vec!["run", "--credentials", "unused"],
        vec![
            "run",
            "--credentials",
            "unused",
            "--state-dir",
            "unused",
            "--width",
            "bad",
        ],
        vec![
            "run",
            "--credentials",
            "unused",
            "--state-dir",
            "unused",
            "--height",
            "bad",
        ],
        vec![
            "run",
            "--credentials",
            "unused",
            "--state-dir",
            "unused",
            "--width",
            "1",
        ],
    ] {
        let result = run(&args);
        assert!(
            !result.status.success(),
            "accepted invalid arguments: {args:?}"
        );
        assert_ne!(result.stderr.len(), 0);
        assert_eq!(result.stdout.len(), 0);
    }
}
