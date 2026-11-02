//! Link-safe directory traversal and atomic configuration I/O.

use std::io;
use std::path::Path;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix {
    use std::ffi::{c_char, c_int, CString, OsStr};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(target_os = "macos")]
    const O_NOFOLLOW: c_int = 0x0000_0100;
    #[cfg(target_os = "macos")]
    const O_DIRECTORY: c_int = 0x0010_0000;
    #[cfg(target_os = "macos")]
    const O_CLOEXEC: c_int = 0x0100_0000;
    #[cfg(target_os = "macos")]
    const O_CREAT: c_int = 0x0000_0200;
    #[cfg(target_os = "macos")]
    const O_EXCL: c_int = 0x0000_0800;
    #[cfg(target_os = "macos")]
    const O_NONBLOCK: c_int = 0x0000_0004;
    #[cfg(target_os = "macos")]
    type Mode = u16;

    #[cfg(target_os = "linux")]
    const O_NOFOLLOW: c_int = 0o400000;
    #[cfg(target_os = "linux")]
    const O_DIRECTORY: c_int = 0o200000;
    #[cfg(target_os = "linux")]
    const O_CLOEXEC: c_int = 0o2000000;
    #[cfg(target_os = "linux")]
    const O_CREAT: c_int = 0o100;
    #[cfg(target_os = "linux")]
    const O_EXCL: c_int = 0o200;
    #[cfg(target_os = "linux")]
    const O_NONBLOCK: c_int = 0o4000;
    #[cfg(target_os = "linux")]
    type Mode = u32;

    const O_WRONLY: c_int = 1;
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" {
        fn openat(dir: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
        fn mkdirat(dir: c_int, path: *const c_char, mode: Mode) -> c_int;
        fn renameat(
            old_dir: c_int,
            old: *const c_char,
            new_dir: c_int,
            new: *const c_char,
        ) -> c_int;
        fn unlinkat(dir: c_int, path: *const c_char, flags: c_int) -> c_int;
    }

    fn name(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
    }

    fn open_at(directory: &File, path: &CString, flags: c_int) -> io::Result<File> {
        // SAFETY: the directory is open and path is NUL-terminated. The fourth
        // argument is used only for O_CREAT and applies restrictive permissions.
        let fd = unsafe { openat(directory.as_raw_fd(), path.as_ptr(), flags, 0o600 as c_int) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh, owned file descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn validate_directory(path: &Path) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path must be absolute",
            ));
        }
        let mut current = File::open("/")?;
        for component in path.components() {
            let part = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::Normal(part) => name(part)?,
                Component::ParentDir => name(OsStr::new(".."))?,
                Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid directory",
                    ))
                }
            };
            current = open_at(&current, &part, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)?;
        }
        Ok(())
    }

    fn parent(path: &Path, create: bool) -> io::Result<(File, CString)> {
        let directory = path
            .parent()
            .filter(|parent| parent.is_absolute())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "configuration path must be absolute",
                )
            })?;
        let basename = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "configuration file name is missing",
            )
        })?;
        let mut current = File::open("/")?;
        for component in directory.components() {
            let part = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::Normal(part) => name(part)?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid directory",
                    ))
                }
            };
            let flags = O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC;
            let next = match open_at(&current, &part, flags) {
                Err(err) if create && err.kind() == io::ErrorKind::NotFound => {
                    // SAFETY: current is an open directory and part is a single
                    // NUL-terminated component, never a path containing '/'.
                    let result = unsafe { mkdirat(current.as_raw_fd(), part.as_ptr(), 0o700) };
                    if result < 0 {
                        let err = io::Error::last_os_error();
                        if err.kind() != io::ErrorKind::AlreadyExists {
                            return Err(err);
                        }
                    }
                    open_at(&current, &part, flags)?
                }
                result => result?,
            };
            current = next;
        }
        Ok((current, name(basename)?))
    }

    pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
        let (directory, basename) = parent(path, false)?;
        let mut file = open_at(&directory, &basename, O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "configuration is not a file",
            ));
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        Ok(data)
    }

    pub(super) fn save(path: &Path, data: &[u8]) -> io::Result<()> {
        let (directory, basename) = parent(path, true)?;
        for _ in 0..128 {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let temporary = CString::new(format!(
                ".oilfind-config-{}-{sequence}.tmp",
                std::process::id()
            ))
            .expect("temporary name is ASCII");
            let flags = O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC;
            let mut file = match open_at(&directory, &temporary, flags) {
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                result => result?,
            };
            let written = file.write_all(data).and_then(|()| file.sync_all());
            drop(file);
            let result = written.and_then(|()| {
                // SAFETY: both names are single components in the same open
                // directory. renameat replaces the destination link itself.
                let status = unsafe {
                    renameat(
                        directory.as_raw_fd(),
                        temporary.as_ptr(),
                        directory.as_raw_fd(),
                        basename.as_ptr(),
                    )
                };
                if status == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
            if result.is_err() {
                // SAFETY: temporary is a single component in the open directory.
                unsafe { unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
            }
            return result;
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate configuration temp file",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn validate_directory(path: &Path) -> io::Result<()> {
    unix::validate_directory(path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    unix::read(path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn save(path: &Path, data: &[u8]) -> io::Result<()> {
    unix::save(path, data)
}

// The preview targets macOS; unsupported platforms fail closed rather than
// falling back to file operations that follow directory links.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn validate_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure directory validation is unavailable",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn read(_path: &Path) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure configuration is unavailable",
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn save(_path: &Path, _data: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure configuration is unavailable",
    ))
}
