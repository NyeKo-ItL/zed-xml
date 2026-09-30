//! Bounded reading of the local resources a document refers to (schemas,
//! DTDs, catalogs, workspace files).
//!
//! Every read made on behalf of a document goes through [`read_text_file`],
//! which enforces the security model of the server:
//!
//! - network paths (`\\server\share`, `//server/share`, Windows device
//!   namespaces) are refused, so that no resolution ever reaches the
//!   network through the file system (SMB/WebDAV);
//! - only regular files are read (symbolic links are followed, but a link
//!   to a device such as `/dev/zero`, a FIFO or a directory is refused);
//! - the size is bounded before and during the read, so a file that grows
//!   while it is read cannot exhaust the memory.

use std::{
    fmt,
    fs::{self, File},
    io::Read,
    path::Path,
};

/// Default maximum size (bytes) of a schema, DTD or catalog read from disk.
pub const MAX_RESOURCE_SIZE: u64 = 16 * 1024 * 1024;

/// Why a local resource was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    /// A network (UNC) or device path.
    NetworkPath,
    /// Not a regular file (directory, device, FIFO, socket).
    NotAFile,
    /// Larger than the limit.
    TooLarge { limit: u64 },
    /// Not valid UTF-8.
    NotUtf8,
    /// I/O error (missing file, permission...).
    Io(String),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkPath => {
                formatter.write_str("network paths are never accessed, use a local copy")
            }
            Self::NotAFile => formatter.write_str("not a regular file"),
            Self::TooLarge { limit } => write!(formatter, "larger than {limit} bytes"),
            Self::NotUtf8 => formatter.write_str("not UTF-8 text"),
            Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ResourceError {}

/// Whether `path` designates a network share or a device namespace rather
/// than a local file: `\\server\share`, `//server/share`, `\\?\UNC\…`,
/// `\\.\device`. Such paths are never read.
pub fn is_network_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let (Some(first), Some(second)) = (chars.next(), chars.next()) else {
        return false;
    };
    let separator = |character: char| character == '/' || character == '\\';
    if !(separator(first) && separator(second)) {
        return false;
    }
    // `\\?\C:\…` is a verbatim local path; `\\?\UNC\…` is a share.
    let rest = &text[2..];
    if let Some(verbatim) = rest.strip_prefix("?\\").or_else(|| rest.strip_prefix("?/")) {
        let bytes = verbatim.as_bytes();
        let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        return !drive;
    }
    true
}

/// Whether `path` is an existing regular file on a local file system
/// (network paths are not probed at all).
pub fn is_local_file(path: &Path) -> bool {
    !is_network_path(path) && path.is_file()
}

/// Reads the regular file `path` as text (UTF-8, or UTF-16/ISO-8859-1 as
/// detected by [`crate::text::decode_bytes`], byte order mark removed), refusing network paths,
/// non-regular files and files larger than `limit` bytes.
pub fn read_text_file(path: &Path, limit: u64) -> Result<String, ResourceError> {
    if is_network_path(path) {
        return Err(ResourceError::NetworkPath);
    }
    let metadata = fs::metadata(path).map_err(|error| ResourceError::Io(error.to_string()))?;
    if !metadata.is_file() {
        return Err(ResourceError::NotAFile);
    }
    if metadata.len() > limit {
        return Err(ResourceError::TooLarge { limit });
    }
    let file = File::open(path).map_err(|error| ResourceError::Io(error.to_string()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| ResourceError::Io(error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(ResourceError::TooLarge { limit });
    }
    crate::text::decode_bytes(&bytes).ok_or(ResourceError::NotUtf8)
}

#[cfg(test)]
mod tests;
