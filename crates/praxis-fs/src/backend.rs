//! The OS-neutral surface: the value types agents see and the
//! [`FsBackend`] trait every implementation provides.

use praxis_core::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One directory entry / stat result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    /// Bytes for files; 0 otherwise.
    pub size: u64,
    /// Modification time, seconds since the Unix epoch (if known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
}

/// Everything is synchronous and may block. Policy (allowed roots,
/// size limits, confirmation) is NOT here — it belongs to the integrator.
pub trait FsBackend: Send + Sync + std::fmt::Debug {
    /// Whole-file contents.
    fn read(&self, path: &Path) -> Result<Vec<u8>>;
    /// Create or truncate `path` and write `data`; with `create_dirs`,
    /// create missing parent directories first.
    fn write(&self, path: &Path, data: &[u8], create_dirs: bool) -> Result<()>;
    /// Append `data` to `path`, creating it if missing.
    fn append(&self, path: &Path, data: &[u8]) -> Result<()>;
    /// Immediate children of a directory, sorted by name.
    fn list(&self, path: &Path) -> Result<Vec<Entry>>;
    /// Metadata of one path (symlinks are not followed).
    fn stat(&self, path: &Path) -> Result<Entry>;
    /// Create a directory; with `recursive`, all missing parents too, and
    /// an existing directory is not an error.
    fn mkdir(&self, path: &Path, recursive: bool) -> Result<()>;
    /// Delete a file or empty directory; with `recursive`, a whole tree.
    fn remove(&self, path: &Path, recursive: bool) -> Result<()>;
    /// Rename / move.
    fn rename(&self, from: &Path, to: &Path) -> Result<()>;
    /// Copy one file (directories are refused).
    fn copy(&self, from: &Path, to: &Path) -> Result<u64>;
    /// Whether anything exists at `path`.
    fn exists(&self, path: &Path) -> bool;
}
