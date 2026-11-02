use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        // The core intentionally rejects the volume-root /tmp directory on
        // Linux; /var/tmp is still temporary but is not a protected root.
        #[cfg(target_os = "linux")]
        let base = PathBuf::from("/var/tmp");
        #[cfg(not(target_os = "linux"))]
        let base = std::env::temp_dir();
        let base = fs::canonicalize(base).expect("canonical temporary directory");
        #[cfg(windows)]
        let base = match base.to_str().and_then(|path| path.strip_prefix(r"\\?\")) {
            Some(path) => PathBuf::from(path),
            None => base,
        };
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "oilfind-mac-test-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("create fixture directory: {err}"),
            }
        }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn app(dir: &TempDir) -> App {
    App::from_storage(Some(dir.join("config.json")), Some(dir.join("cache.bin")))
}

#[test]
fn first_launch_stays_empty_and_does_not_scan_or_create_configuration() {
    let dir = TempDir::new();
    let app = app(&dir);
    assert!(app.roots.is_empty());
    assert!(app.index.is_none());
    assert!(!app.scanning);
    assert!(app.rx.is_none());
    assert!(!dir.join("config.json").exists());
}

#[test]
fn overlapping_roots_are_deduplicated_in_both_orders() {
    let dir = TempDir::new();
    let parent = dir.join("photos");
    let child = parent.join("2024");
    let sibling = dir.join("photos-old");
    let mut roots = Vec::new();
    assert!(insert_root(&mut roots, child.clone()));
    assert!(insert_root(&mut roots, parent.clone()));
    assert_eq!(roots, vec![parent.clone()]);
    assert!(!insert_root(&mut roots, child));
    assert!(insert_root(&mut roots, sibling.clone()));
    assert_eq!(roots, vec![parent, sibling]);

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        fs::create_dir(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("photos/2024")).unwrap();
        let cleaned = clean_root(&dir.join("a/../photos/./2024")).expect("absolute root");
        assert_eq!(cleaned, dir.join("photos/2024"));
    }
    assert!(clean_root(Path::new("relative/path")).is_none());
}

#[test]
fn cache_root_comparison_is_order_independent_but_case_sensitive() {
    let dir = TempDir::new();
    let first = dir.join("First");
    let second = dir.join("Second");
    assert!(same_roots(
        &[first.clone(), second.clone()],
        &[second, first.clone()]
    ));
    assert!(!same_roots(&[first], &[dir.join("first")]));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_config(dir: &TempDir, roots: Vec<PathBuf>, cache_roots: Vec<PathBuf>) {
    let data = serde_json::to_vec(&Config {
        version: CONFIG_VERSION,
        roots,
        cache_roots,
    })
    .expect("encode config");
    safe_config::save(&dir.join("config.json"), &data).expect("save config");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_cache(dir: &TempDir, roots: Vec<PathBuf>) {
    Index {
        roots,
        entries: Vec::new(),
        built_at: 0,
    }
    .save(&dir.join("cache.bin"))
    .expect("save cache");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn linked_parent_fixture(dir: &TempDir) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::symlink;

    let lexical = dir.join("allowed");
    let target = dir.join("elsewhere");
    fs::create_dir(&lexical).unwrap();
    fs::create_dir_all(target.join("nested")).unwrap();
    fs::create_dir(target.join("allowed")).unwrap();
    symlink(target.join("nested"), dir.join("link")).unwrap();
    let raw = dir.join("link/../allowed");
    assert_eq!(fs::canonicalize(&raw).unwrap(), target.join("allowed"));
    (raw, lexical)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn choosing_a_path_through_symlink_dotdot_is_rejected() {
    let dir = TempDir::new();
    let (raw, _) = linked_parent_fixture(&dir);
    let mut preview = app(&dir);
    preview.add_selected_root(raw);
    assert!(preview.roots.is_empty());
    assert!(!preview.scanning);
    assert!(preview.notice.contains("链接"));
    assert!(!dir.join("config.json").exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn symlink_dotdot_in_saved_scope_cannot_authorize_a_cache() {
    let dir = TempDir::new();
    let (raw, lexical) = linked_parent_fixture(&dir);
    write_config(&dir, vec![raw], vec![lexical.clone()]);
    write_cache(&dir, vec![lexical]);
    let original_config = fs::read(dir.join("config.json")).unwrap();
    let original_cache = fs::read(dir.join("cache.bin")).unwrap();
    let preview = app(&dir);
    assert!(preview.roots.is_empty());
    assert!(preview.index.is_none());
    assert!(!preview.scanning);
    assert_eq!(fs::read(dir.join("config.json")).unwrap(), original_config);
    assert_eq!(fs::read(dir.join("cache.bin")).unwrap(), original_cache);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn missing_component_before_dotdot_is_not_normalized_away() {
    let dir = TempDir::new();
    fs::create_dir(dir.join("allowed")).unwrap();
    let mut preview = app(&dir);
    preview.add_selected_root(dir.join("missing/../allowed"));
    assert!(preview.roots.is_empty());
    assert!(!preview.scanning);
    assert!(!dir.join("config.json").exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn unreadable_component_before_dotdot_is_rejected() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new();
    let inaccessible = dir.join("inaccessible");
    fs::create_dir(&inaccessible).unwrap();
    fs::create_dir(dir.join("allowed")).unwrap();
    let original = fs::metadata(&inaccessible).unwrap().permissions();
    fs::set_permissions(&inaccessible, fs::Permissions::from_mode(0o000)).unwrap();
    let denied = fs::File::open(&inaccessible).is_err();
    let mut preview = app(&dir);
    preview.add_selected_root(inaccessible.join("../allowed"));
    fs::set_permissions(&inaccessible, original).unwrap();
    // Privileged test runners may be able to read a mode-000 directory.
    if denied {
        assert!(preview.roots.is_empty());
        assert!(!preview.scanning);
        assert!(!dir.join("config.json").exists());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn legacy_cache_is_ignored_until_selection_and_explicit_scan() {
    let dir = TempDir::new();
    let root = dir.join("selected");
    fs::create_dir(&root).expect("create selected directory");
    fs::write(root.join("sample.txt"), b"fixture").expect("write fixture");
    let legacy = serde_json::to_vec(&serde_json::json!({"roots": [root]})).unwrap();
    fs::write(dir.join("config.json"), &legacy).expect("write legacy config");
    write_cache(&dir, vec![root.clone()]);
    let old_cache = fs::read(dir.join("cache.bin")).expect("read legacy cache");

    let mut preview = app(&dir);
    assert!(preview.roots.is_empty());
    assert!(preview.index.is_none());
    assert!(!preview.scanning);
    assert!(preview.notice.contains("重新选择"));
    assert_eq!(fs::read(dir.join("config.json")).unwrap(), legacy);
    assert_eq!(fs::read(dir.join("cache.bin")).unwrap(), old_cache);

    preview.add_selected_root(root.clone());
    assert_eq!(preview.roots, vec![root.clone()]);
    assert!(!preview.scanning);
    assert_eq!(fs::read(dir.join("cache.bin")).unwrap(), old_cache);
    let before_scan = app(&dir);
    assert!(before_scan.index.is_none());
    assert!(!before_scan.scanning);
    assert!(before_scan.notice.contains("开始索引"));

    // This is the same action as the start button, on a temporary fixture only.
    preview.start_scan();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while preview.scanning && std::time::Instant::now() < deadline {
        preview.poll_scan();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!preview.scanning, "fixture scan did not finish");
    assert!(preview.index.is_some(), "{}", preview.status);
    assert_eq!(preview.cache_roots, vec![root.clone()]);
    assert_ne!(fs::read(dir.join("cache.bin")).unwrap(), old_cache);

    let restarted = app(&dir);
    assert_eq!(restarted.roots, vec![root]);
    assert!(restarted.index.is_some());
    assert!(!restarted.scanning);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn failed_cache_save_does_not_authorize_a_cache() {
    let dir = TempDir::new();
    let root = dir.join("selected");
    let blocker = dir.join("cache-directory");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&blocker).unwrap();
    let mut preview = App::from_storage(Some(dir.join("config.json")), Some(blocker));
    preview.add_selected_root(root.clone());
    let (sender, receiver) = mpsc::channel();
    preview.scanning = true;
    preview.rx = Some(receiver);
    assert!(sender
        .send(ScanMsg::Done(Ok(Index {
            roots: vec![root],
            entries: Vec::new(),
            built_at: 0,
        })))
        .is_ok());
    preview.poll_scan();

    assert!(preview.index.is_some());
    assert!(preview.cache_roots.is_empty());
    assert!(preview.status.contains("缓存未保存"));
    let config: Config =
        serde_json::from_slice(&fs::read(dir.join("config.json")).unwrap()).unwrap();
    assert!(config.cache_roots.is_empty());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn matching_cache_loads_after_parent_child_roots_are_deduplicated() {
    let dir = TempDir::new();
    let parent = dir.join("selected");
    let child = parent.join("child");
    fs::create_dir_all(&child).expect("create selected directories");
    write_config(
        &dir,
        vec![child, parent.clone(), parent.clone()],
        vec![parent.clone()],
    );
    write_cache(&dir, vec![parent.clone()]);

    let app = app(&dir);
    assert_eq!(app.roots, vec![parent]);
    assert!(app.index.is_some());
    assert!(!app.scanning);
    assert!(app.rx.is_none());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn missing_mismatched_and_corrupt_caches_never_start_scanning() {
    let dir = TempDir::new();
    let root = dir.join("selected");
    let other = dir.join("other");
    fs::create_dir(&root).expect("create selected directory");
    fs::create_dir(&other).expect("create other directory");
    write_config(&dir, vec![root.clone()], vec![root.clone()]);

    let missing = app(&dir);
    assert_eq!(missing.roots, vec![root.clone()]);
    assert!(missing.index.is_none());
    assert!(!missing.scanning);

    write_cache(&dir, vec![other]);
    let mismatched = app(&dir);
    assert!(mismatched.index.is_none());
    assert!(!mismatched.scanning);
    assert!(mismatched.notice.contains("不一致"));

    fs::write(dir.join("cache.bin"), b"not a cache").expect("write corrupt fixture");
    let corrupt = app(&dir);
    assert!(corrupt.index.is_none());
    assert!(!corrupt.scanning);
    assert!(corrupt.notice.contains("未加载缓存"));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn cached_root_with_a_new_symlinked_ancestor_is_not_loaded() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new();
    let parent = dir.join("parent");
    let root = parent.join("selected");
    let other = dir.join("other");
    fs::create_dir_all(&root).expect("create selected directory");
    fs::create_dir_all(other.join("selected")).expect("create other directory");
    write_config(&dir, vec![root.clone()], vec![root.clone()]);
    write_cache(&dir, vec![root.clone()]);
    let original_config = fs::read(dir.join("config.json")).unwrap();
    let original_cache = fs::read(dir.join("cache.bin")).unwrap();
    fs::remove_dir(&root).unwrap();
    fs::remove_dir(&parent).unwrap();
    symlink(&other, &parent).expect("replace ancestor with a symlink");

    let preview = app(&dir);
    assert!(preview.roots.is_empty());
    assert!(preview.index.is_none());
    assert!(!preview.scanning);
    assert!(preview.notice.contains("链接"));
    assert_eq!(fs::read(dir.join("config.json")).unwrap(), original_config);
    assert_eq!(fs::read(dir.join("cache.bin")).unwrap(), original_cache);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn oversized_configuration_does_not_partially_authorize_a_cache() {
    let dir = TempDir::new();
    let root = dir.join("selected");
    fs::create_dir(&root).expect("create selected directory");
    write_config(&dir, vec![root.clone(); MAX_ROOTS + 1], vec![root.clone()]);
    write_cache(&dir, vec![root]);
    let original = fs::read(dir.join("config.json")).unwrap();
    let preview = app(&dir);
    assert!(preview.roots.is_empty());
    assert!(preview.index.is_none());
    assert!(!preview.scanning);
    assert!(preview.notice.contains("超出安全上限"));
    assert_eq!(fs::read(dir.join("config.json")).unwrap(), original);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn deleting_all_roots_stays_empty_after_restart() {
    let dir = TempDir::new();
    let root = dir.join("selected");
    fs::create_dir(&root).expect("create selected directory");
    write_config(&dir, vec![root.clone()], vec![root.clone()]);
    write_cache(&dir, vec![root]);
    let mut initial = app(&dir);
    assert!(initial.index.is_some());

    initial.remove_root(0);
    assert!(initial.roots.is_empty());
    assert!(initial.index.is_none());
    assert!(!initial.scanning);
    let restarted = app(&dir);
    assert!(restarted.roots.is_empty());
    assert!(restarted.index.is_none());
    assert!(!restarted.scanning);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn config_save_replaces_symlink_and_hardlink_without_modifying_targets() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let dir = TempDir::new();
    let config = dir.join("config.json");
    let target = dir.join("other-file");
    fs::write(&target, b"preserve me").expect("write target");
    symlink(&target, &config).expect("create config symlink");
    assert!(safe_config::read(&config).is_err());
    let root = dir.join("selected");
    let mut preview = App::from_storage(Some(config.clone()), None);
    preview.roots = vec![root.clone()];
    preview.save_config();
    assert_eq!(fs::read(&target).expect("read target"), b"preserve me");
    let saved: Config = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(saved.roots, vec![root.clone()]);
    assert!(!fs::symlink_metadata(&config)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o077,
        0
    );

    fs::remove_file(&config).expect("remove config");
    fs::hard_link(&target, &config).expect("create config hard link");
    preview.save_config();
    assert_eq!(fs::read(&target).expect("read target"), b"preserve me");
    let saved: Config = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(saved.roots, vec![root]);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn config_io_rejects_symlinked_parent_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new();
    let target_dir = dir.join("other-directory");
    fs::create_dir(&target_dir).expect("create target directory");
    let target = target_dir.join("config.json");
    fs::write(&target, b"preserve me").expect("write target");
    let link = dir.join("linked-directory");
    symlink(&target_dir, &link).expect("create directory symlink");
    let config = link.join("config.json");
    assert!(safe_config::read(&config).is_err());
    let mut preview = App::from_storage(Some(config), None);
    preview.roots = vec![dir.join("selected")];
    preview.save_config();
    assert!(preview.notice.contains("保存目录配置失败"));
    assert_eq!(fs::read(&target).expect("read target"), b"preserve me");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn preexisting_temp_symlinks_cannot_redirect_config_writes() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new();
    let target = dir.join("other-file");
    fs::write(&target, b"preserve me").expect("write target");
    // Exhaust the bounded sequence space used by the concurrently running tests.
    for sequence in 0..512 {
        let temporary = dir.join(&format!(
            ".oilfind-config-{}-{sequence}.tmp",
            std::process::id()
        ));
        symlink(&target, temporary).expect("create temporary symlink");
    }
    assert!(safe_config::save(&dir.join("config.json"), b"replacement").is_err());
    assert_eq!(fs::read(&target).expect("read target"), b"preserve me");
    assert!(!dir.join("config.json").exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn failed_atomic_replace_cleans_the_temp_file() {
    let dir = TempDir::new();
    let config = dir.join("config.json");
    fs::create_dir(&config).expect("create blocking directory");
    assert!(safe_config::save(&config, b"replacement").is_err());
    assert!(config.is_dir());
    let remaining = fs::read_dir(&dir.0).expect("read fixture").count();
    assert_eq!(remaining, 1);
}
