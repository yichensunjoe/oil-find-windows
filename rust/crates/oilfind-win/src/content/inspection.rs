//! Inspect read-only handles within an approved local volume before reading bytes.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileStamp {
    pub(super) bytes: u64,
    modified: Duration,
    before_epoch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<FileIdentity>,
}

impl FileStamp {
    pub(super) fn from_file(file: &File) -> io::Result<Self> {
        Self::from_opened(file, &file.metadata()?)
    }

    fn from_opened(file: &File, metadata: &fs::Metadata) -> io::Result<Self> {
        if !metadata.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "只读取普通文件"));
        }
        let time = metadata.modified()?.duration_since(UNIX_EPOCH);
        Ok(Self {
            bytes: metadata.len(),
            before_epoch: time.is_err(),
            modified: time.unwrap_or_else(|err| err.duration()),
            identity: file_identity(file, metadata)?,
        })
    }

    pub(super) fn matches_metadata(&self, metadata: &fs::Metadata) -> bool {
        self.bytes == metadata.len()
            && metadata.modified().is_ok_and(|time| {
                let elapsed = time.duration_since(UNIX_EPOCH);
                self.before_epoch == elapsed.is_err()
                    && self.modified == elapsed.unwrap_or_else(|err| err.duration())
            })
    }

    pub(super) fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    pub(super) fn reusable_as(&self, current: &Self) -> bool {
        self.has_identity() && self == current
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
enum FileIdentity {
    Windows {
        volume: u64,
        file: [u8; 16],
        creation_time: i64,
        change_time: i64,
    },
    Unix {
        device: u64,
        inode: u64,
        change_seconds: i64,
        change_nanos: i64,
    },
}

pub(super) fn inspect(
    path: &Path,
    volumes: &BTreeMap<String, String>,
) -> io::Result<(File, FileStamp)> {
    let (file, metadata) = open(path, volumes)?;
    let stamp = FileStamp::from_opened(&file, &metadata)?;
    Ok((file, stamp))
}

pub(super) fn is_inaccessible(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
        if err.raw_os_error().is_some_and(|code| {
            matches!(code as u32, ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        }) {
            return true;
        }
    }
    err.kind() == io::ErrorKind::PermissionDenied
}

#[cfg(windows)]
fn file_identity(file: &File, _: &fs::Metadata) -> io::Result<Option<FileIdentity>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{
        ERROR_CALL_NOT_IMPLEMENTED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    fn information<T: Default>(
        file: &File,
        class: windows_sys::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
    ) -> io::Result<Option<T>> {
        let mut information = T::default();
        // Both information classes below are queried into their matching C structs.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                class,
                (&mut information as *mut T).cast(),
                std::mem::size_of::<T>() as u32,
            )
        };
        if success != 0 {
            return Ok(Some(information));
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error().is_some_and(|code| {
            matches!(
                code as u32,
                ERROR_INVALID_FUNCTION
                    | ERROR_NOT_SUPPORTED
                    | ERROR_INVALID_PARAMETER
                    | ERROR_CALL_NOT_IMPLEMENTED
            )
        }) {
            // Filesystems without a reliable identity stay readable and get re-extracted.
            Ok(None)
        } else {
            Err(err)
        }
    }

    let Some(id): Option<FILE_ID_INFO> = information(file, FileIdInfo)? else {
        return Ok(None);
    };
    let Some(basic): Option<FILE_BASIC_INFO> = information(file, FileBasicInfo)? else {
        return Ok(None);
    };
    let id_bytes = id.FileId.Identifier;
    Ok(
        (id_bytes != [0; 16] && basic.ChangeTime != 0).then_some(FileIdentity::Windows {
            volume: id.VolumeSerialNumber,
            file: id_bytes,
            creation_time: basic.CreationTime,
            change_time: basic.ChangeTime,
        }),
    )
}

#[cfg(unix)]
fn file_identity(_: &File, metadata: &fs::Metadata) -> io::Result<Option<FileIdentity>> {
    use std::os::unix::fs::MetadataExt;

    Ok(Some(FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
        change_seconds: metadata.ctime(),
        change_nanos: metadata.ctime_nsec(),
    }))
}

#[cfg(not(any(windows, unix)))]
fn file_identity(_: &File, _: &fs::Metadata) -> io::Result<Option<FileIdentity>> {
    Ok(None)
}

#[cfg(windows)]
pub(super) fn open(
    path: &Path,
    volumes: &BTreeMap<String, String>,
) -> io::Result<(File, fs::Metadata)> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        NtOpenFile, FILE_NON_DIRECTORY_FILE, FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT,
        FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::{
        RtlNtStatusToDosError, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_FILE_IS_A_DIRECTORY,
        STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_SUCCESS, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_TRAVERSE, VOLUME_NAME_GUID,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const UNSAFE_ATTRIBUTES: u32 = FILE_ATTRIBUTE_REPARSE_POINT
        | FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
    let input = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "文件路径无效"))?;
    let input = input.replace('/', "\\");
    let (drive, relative) = input
        .split_once('\\')
        .filter(|(drive, relative)| {
            drive.len() == 2
                && drive.as_bytes()[1] == b':'
                && !relative.is_empty()
                && !relative.contains([':', '\0'])
                && relative
                    .split('\\')
                    .all(|part| !matches!(part, "" | "." | ".."))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "文件路径无效"))?;
    let drive = format!("{}\\", drive.to_ascii_uppercase());
    let volume = volumes
        .get(&drive)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "文件所在磁盘未经确认"))?;

    // The GUID names a local volume confirmed by GetVolumeNameForVolumeMountPointW.
    // Open that root first, so no-follow parsing begins below NT's DOS alias.
    let root = fs::OpenOptions::new()
        .access_mode(FILE_TRAVERSE | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
        )
        .open(volume)?;
    let metadata = root.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & UNSAFE_ATTRIBUTES != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "无法验证本机磁盘",
        ));
    }
    let mut nt_path: Vec<u16> = relative.encode_utf16().collect();
    let length = u16::try_from(nt_path.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "文件路径过长"))?;
    let name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: nt_path.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let mut handle = std::ptr::null_mut();
    let mut information = IO_STATUS_BLOCK::default();
    // The root handle and buffers stay alive for the synchronous request.
    // Reject reparse points in the path relative to that root handle.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            FILE_GENERIC_READ,
            &attributes,
            &mut information,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | FILE_OPEN_NO_RECALL,
        )
    };
    if status == STATUS_REPARSE_POINT_ENCOUNTERED {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "不读取链接或重解析点",
        ));
    }
    if status == STATUS_FILE_IS_A_DIRECTORY {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "文件已变成目录",
        ));
    }
    if status != STATUS_SUCCESS {
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    if handle.is_null() {
        return Err(io::Error::other("无法打开文件"));
    }
    // Successful NtOpenFile transfers ownership of the handle.
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & UNSAFE_ATTRIBUTES != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "不读取链接或离线文件",
        ));
    }

    // Validate the opened handle before reading any bytes. This prevents a
    // junction swapped in after the metadata scan from redirecting a body read.
    let mut buffer = [0; 512];
    let read_path = |buffer: &mut [u16]| unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_GUID,
        ) as usize
    };
    let length = read_path(&mut buffer);
    let resolved = if length >= buffer.len() && length <= 32_768 {
        let mut large = vec![0; length + 1];
        let length = read_path(&mut large);
        if length == 0 || length >= large.len() {
            return Err(io::Error::other("无法验证已打开的文件"));
        }
        String::from_utf16_lossy(&large[..length])
    } else if length > 0 && length < buffer.len() {
        String::from_utf16_lossy(&buffer[..length])
    } else {
        return Err(io::Error::other("无法验证已打开的文件"));
    };
    let expected_volume = volume.trim_end_matches('\\');
    if !resolved
        .get(..expected_volume.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected_volume))
        || resolved
            .get(expected_volume.len()..)
            .and_then(|path| path.strip_prefix('\\'))
            != Some(relative)
    {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "文件位置已变化"));
    }
    Ok((file, metadata))
}

#[cfg(not(windows))]
pub(super) fn open(path: &Path, _: &BTreeMap<String, String>) -> io::Result<(File, fs::Metadata)> {
    let initial = fs::symlink_metadata(path)?;
    if !initial.is_file() || fs::canonicalize(path)? != path {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "文件位置已变化"));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0o400000 | 0o4000); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0o100000 | 0o4000); // O_NOFOLLOW | O_NONBLOCK
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || fs::canonicalize(path)? != path {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "文件位置已变化"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (metadata.dev(), metadata.ino()) != (initial.dev(), initial.ino()) {
            return Err(io::Error::other("读取正文时文件发生变化"));
        }
    }
    Ok((file, metadata))
}
