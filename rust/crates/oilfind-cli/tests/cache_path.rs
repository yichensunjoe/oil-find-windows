//! The CLI must preserve cache files and directories when a scan fails.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Self {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(&base).expect("create target temp directory");
        let base = fs::canonicalize(base).expect("canonicalize target temp directory");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = base.join(format!(
            "oilfind-cli-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create fixture");
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scan_does_not_create_cache_directories_through_a_link() {
    let fixture = Fixture::new("cache-link");
    let root = fixture.0.join("selected");
    let outside = fixture.0.join("outside");
    fs::create_dir(&root).expect("create selected directory");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(root.join("sample.txt"), b"fixture").expect("create sample");
    let link = fixture.0.join("linked-cache");
    symlink(&outside, &link).expect("create link");

    let output = Command::new(env!("CARGO_BIN_EXE_oilfind-cli"))
        .args(["-scan", "-root"])
        .arg(&root)
        .arg("-cache")
        .arg(link.join("nested/index.bin"))
        .output()
        .expect("run CLI");
    assert!(!output.status.success(), "linked cache was accepted");
    assert!(String::from_utf8_lossy(&output.stderr).contains("链接"));
    assert!(
        !outside.join("nested").exists(),
        "CLI created a directory through a link"
    );
}

#[test]
fn unreadable_root_does_not_replace_the_previous_cache() {
    let fixture = Fixture::new("unreadable-root");
    let root = fixture.0.join("selected");
    let cache = fixture.0.join("index.bin");
    fs::create_dir(&root).expect("create selected directory");
    fs::write(root.join("sample.txt"), b"fixture").expect("create sample");

    let mut scan = Command::new(env!("CARGO_BIN_EXE_oilfind-cli"));
    scan.args(["-scan", "-root"])
        .arg(&root)
        .arg("-cache")
        .arg(&cache);
    let first = scan.output().expect("create initial cache");
    assert!(first.status.success(), "{:?}", first);
    let original = fs::read(&cache).expect("read initial cache");

    let permissions = fs::metadata(&root)
        .expect("read root metadata")
        .permissions();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).expect("deny directory reads");
    let denied = fs::read_dir(&root).is_err();
    let second = denied.then(|| scan.output());
    fs::set_permissions(&root, permissions).expect("restore fixture permissions");
    let Some(second) = second else {
        // Elevated users can still enumerate the directory.
        return;
    };

    let output = second.expect("run CLI with unreadable root");
    assert!(
        !output.status.success(),
        "rescan reported success: {output:?}"
    );
    assert_eq!(
        fs::read(&cache).expect("read cache after failure"),
        original
    );
}
