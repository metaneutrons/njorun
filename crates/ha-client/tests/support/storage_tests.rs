use super::*;

#[test]
fn failed_directory_sync_reports_replacement_without_claiming_durability() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("njorun-sync-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).expect("fixture directory");
    let path = directory.join("state.json");
    write(&path, &1_u32).expect("initial durable state");
    let result = write_with_directory_sync(&path, &2_u32, 65536, |_| {
        Err(std::io::Error::other("injected directory sync failure"))
    });
    assert_eq!(result, Err(ClientError::StorageCommitUncertain));
    assert_eq!(read::<u32>(&path).expect("visible replacement"), 2);
    write(&path, &2_u32).expect("retry establishes durability");
    assert_eq!(read::<u32>(&path).expect("durable replacement"), 2);
    fs::remove_dir_all(directory).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn private_reads_reject_group_or_world_access_on_the_opened_file() {
    use std::os::unix::fs::PermissionsExt;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("njorun-private-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).expect("fixture directory");
    let path = directory.join("credentials.json");
    write(&path, &42_u32).expect("private credentials");
    assert_eq!(read_private::<u32>(&path).expect("private read"), 42);
    for mode in [0o644, 0o640, 0o606] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("unsafe fixture mode");
        assert_eq!(read_private::<u32>(&path), Err(ClientError::Storage));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("read-only private mode");
    assert_eq!(
        read_private::<u32>(&path).expect("read-only private read"),
        42
    );
    fs::remove_dir_all(directory).expect("cleanup");
}
