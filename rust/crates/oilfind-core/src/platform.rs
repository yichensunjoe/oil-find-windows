//! Platform specific pieces: link detection, drive validation and atomic
//! cache replacement.
//!
//! Only the Win32 calls the Go implementation uses are declared here, so the
//! crate does not need a Windows binding dependency.

use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
#[cfg(windows)]
const DRIVE_REMOVABLE: u32 = 2;
#[cfg(windows)]
const DRIVE_FIXED: u32 = 3;
#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

/// True when the entry must never be followed: a symbolic link, or on Windows
/// any reparse point (junction, mount point, app-exec alias, ...).
pub(crate) fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Rejects anything that is not a local fixed or removable volume.
///
/// The lexical checks (absolute, no UNC, no device path) happen in
/// [`crate::scan`] because they apply on every platform; this function only
/// asks Windows for the volume type. On other platforms it is a no-op.
pub(crate) fn validate_local_volume(path: &Path) -> Result<(), crate::Error> {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
            return Err(crate::Error::NetworkRoot);
        }
        let drive = [
            text.as_bytes()[0] as u16,
            u16::from(b':'),
            u16::from(b'\\'),
            0,
        ];
        // SAFETY: `drive` is a NUL terminated UTF-16 buffer owned by this stack
        // frame and `GetDriveTypeW` only reads it.
        let kind = unsafe { GetDriveTypeW(drive.as_ptr()) };
        if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
            return Err(crate::Error::NetworkRoot);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

/// Atomically replaces `dest` with `source`, both in the same directory.
pub(crate) fn replace_file(source: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let source: Vec<u16> = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let dest: Vec<u16> = dest
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: both buffers are NUL terminated UTF-16 owned by this frame and
        // MoveFileExW only reads them.
        let ok = unsafe {
            MoveFileExW(
                source.as_ptr(),
                dest.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(source, dest)
    }
}

/// Case-insensitive comparison key for a path, used for root de-duplication.
///
/// Both Windows and macOS file systems are case-insensitive by default and the
/// product only targets Windows, so the comparison is case-insensitive
/// everywhere; separators are normalised on Windows.
pub(crate) fn compare_key(path: &Path) -> String {
    let text = path.to_string_lossy().to_lowercase();
    #[cfg(windows)]
    let text = text.replace('/', "\\");
    let separator = if cfg!(windows) { '\\' } else { '/' };
    text.trim_end_matches(separator).to_string()
}

/// True when `path` is `root` or lies inside it, compared per path segment.
pub(crate) fn path_under(path: &Path, root: &Path) -> bool {
    let path_key = compare_key(path);
    let root_key = compare_key(root);
    let separator = if cfg!(windows) { '\\' } else { '/' };
    path_key == root_key || path_key.starts_with(&format!("{root_key}{separator}"))
}

/// True when the path is a volume root (`/` or `C:\`).
pub(crate) fn is_volume_root(path: &Path) -> bool {
    let key = compare_key(path);
    if key.is_empty() {
        return true;
    }
    cfg!(windows) && key.len() == 2 && key.ends_with(':')
}

/// Lexically cleans a path the way Go's `filepath.Clean` does, without touching
/// the file system (so it never resolves a link).
pub(crate) fn clean_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => cleaned.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(part) => cleaned.push(part),
        }
    }
    if cleaned.as_os_str().is_empty() {
        cleaned.push(std::path::MAIN_SEPARATOR_STR);
    }
    cleaned
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_under_respects_segment_boundaries() {
        let root = Path::new("/data/photos");
        assert!(path_under(Path::new("/data/photos"), root));
        assert!(path_under(Path::new("/data/photos/2024"), root));
        assert!(!path_under(Path::new("/data/photos-old"), root));
    }

    #[test]
    fn clean_path_collapses_dots() {
        assert_eq!(clean_path(Path::new("/a/b/./c")), PathBuf::from("/a/b/c"));
        assert_eq!(clean_path(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(clean_path(Path::new("/..")), PathBuf::from("/"));
    }
}
