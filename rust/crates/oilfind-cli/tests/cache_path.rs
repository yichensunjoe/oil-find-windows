//! The CLI must validate cache paths before creating their parent directories.

#[cfg(unix)]
#[test]
fn scan_does_not_create_cache_directories_through_a_link() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).expect("create target temp directory");
    let base = fs::canonicalize(base).expect("canonicalize target temp directory");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let fixture = Fixture(base.join(format!("oilfind-cli-{}-{unique}", std::process::id())));
    fs::create_dir(&fixture.0).expect("create fixture");
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
