//! Errors returned by scanning, searching and caching.
//!
//! Messages are user facing: the UI and the CLI render [`Display`] directly, so
//! they are kept in the same wording as the Go implementation this crate ports.

use std::fmt;
use std::io;

/// Everything that can go wrong in this crate.
#[derive(Debug)]
pub enum Error {
    /// No directory was supplied.
    EmptyRoots,
    /// More than [`crate::MAX_ROOTS`] directories were supplied.
    TooManyRoots,
    /// A directory entry was blank.
    EmptyRoot,
    /// A directory was not an absolute path.
    RelativeRoot,
    /// UNC shares, device paths and network drives are not supported.
    NetworkRoot,
    /// A root was a file instead of a directory.
    NotDirectory,
    /// A root (or one of its ancestors) is a symbolic link or reparse point.
    LinkRoot,
    /// A root (or one of its ancestors) is a system, dependency or agent directory.
    ProtectedRoot,
    /// A root could not be inspected.
    RootUnreadable(io::Error),
    /// The index reached [`crate::MAX_ENTRIES`].
    EntryLimit,
    /// The caller cancelled the operation.
    Cancelled,
    /// An entry read from a cache is malformed.
    InvalidEntry,
    /// A root read from a cache is malformed.
    InvalidRoot,
    /// The cache file would exceed [`crate::MAX_CACHE_BYTES`].
    CacheTooLarge,
    /// The cache file is not a regular file or is a link/reparse point.
    CacheUnsupported(&'static str),
    /// The cache file is corrupt, truncated or written by another version.
    CacheCorrupt(String),
    /// An I/O operation failed.
    Io(io::Error),
    /// The query string exceeds 4096 bytes.
    QueryTooLong,
    /// The query has more than 128 conditions.
    TooManyTerms,
    /// `|` appeared with an empty side.
    EmptyOr,
    /// A condition was empty.
    EmptyTerm,
    /// A quote was opened but never closed.
    UnclosedQuote,
    /// A regular expression did not compile.
    InvalidRegex(String),
    /// A size condition could not be parsed.
    InvalidSize,
    /// A size range was reversed or unparsable.
    InvalidSizeRange,
    /// A date was not `YYYY-MM-DD`.
    InvalidDate,
    /// A date range was reversed.
    InvalidDateRange,
    /// An `ext:` condition had an empty extension.
    InvalidExtension,
    /// A `kind:` condition used an unknown category.
    UnsupportedKind(String),
    /// A `path:` condition was empty.
    EmptyPath,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::EmptyRoots => f.write_str("请先选择至少一个本机目录"),
            Error::TooManyRoots => write!(f, "最多支持 {} 个索引目录", crate::MAX_ROOTS),
            Error::EmptyRoot => f.write_str("目录不能为空"),
            Error::RelativeRoot => f.write_str("索引目录必须是绝对路径"),
            Error::NetworkRoot => f.write_str("首版不支持网络目录或设备路径"),
            Error::NotDirectory => f.write_str("索引范围必须是目录"),
            Error::LinkRoot => f.write_str("为避免越界，不能索引符号链接或目录联接"),
            Error::ProtectedRoot => {
                f.write_str("该目录属于系统、依赖或私人运行器目录，不在首版索引范围内")
            }
            Error::RootUnreadable(err) => write!(f, "无法访问索引目录: {err}"),
            Error::EntryLimit => write!(
                f,
                "索引达到 {} 条安全上限，请缩小目录范围",
                crate::MAX_ENTRIES
            ),
            Error::Cancelled => f.write_str("操作已取消"),
            Error::InvalidEntry => f.write_str("索引条目损坏"),
            Error::InvalidRoot => f.write_str("索引目录损坏"),
            Error::CacheTooLarge => f.write_str("索引缓存超过 512MB 上限，请缩小索引范围"),
            Error::CacheUnsupported(reason) => f.write_str(reason),
            Error::CacheCorrupt(detail) => write!(f, "索引缓存无法读取: {detail}"),
            Error::Io(err) => write!(f, "{err}"),
            Error::QueryTooLong => f.write_str("查询最多支持 4096 字节"),
            Error::TooManyTerms => f.write_str("查询条件过多"),
            Error::EmptyOr => f.write_str("OR 两侧不能为空"),
            Error::EmptyTerm => f.write_str("条件不能为空"),
            Error::UnclosedQuote => f.write_str("查询引号未闭合"),
            Error::InvalidRegex(detail) => write!(f, "正则表达式无效: {detail}"),
            Error::InvalidSize => f.write_str("大小条件无效"),
            Error::InvalidSizeRange => f.write_str("大小范围无效"),
            Error::InvalidDate => f.write_str("日期格式应为 YYYY-MM-DD"),
            Error::InvalidDateRange => f.write_str("日期范围无效"),
            Error::InvalidExtension => f.write_str("扩展名不能为空"),
            Error::UnsupportedKind(kind) => write!(f, "不支持的文件类型: {kind}"),
            Error::EmptyPath => f.write_str("路径条件不能为空"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::RootUnreadable(err) | Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}
