//! The real backend: a thin, policy-free wrapper over `std::fs`.

use crate::backend::{Entry, EntryKind, FsBackend};
use praxis_core::{Error, Result};
use std::path::Path;

#[cfg(unix)]
const EXDEV: i32 = 18;
#[cfg(not(unix))]
const EXDEV: i32 = 17;

#[derive(Debug, Default)]
pub struct RealFs;

impl RealFs {
    pub fn new() -> Self {
        Self
    }
}

fn io<T>(what: &str, path: &Path, r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| {
        let base = Error::from_io(&e);
        let msg = format!("{what} {}: {e}", path.display());
        match base {
            Error::NotFound(_) => Error::not_found(msg),
            Error::PermissionDenied(_) => Error::perm(msg),
            _ => Error::io(msg),
        }
    })
}

pub(crate) fn entry_from(path: &Path, md: &std::fs::Metadata) -> Entry {
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Entry {
        path: path.to_path_buf(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind,
        size: if kind == EntryKind::File { md.len() } else { 0 },
        modified: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
    }
}

impl FsBackend for RealFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        io("read", path, std::fs::read(path))
    }

    fn read_prefix(&self, path: &Path, max: usize) -> Result<Vec<u8>> {
        use std::io::Read;
        let f = io("read", path, std::fs::File::open(path))?;
        let mut buf = Vec::new();
        io("read", path, f.take(max as u64).read_to_end(&mut buf))?;
        Ok(buf)
    }

    fn write(&self, path: &Path, data: &[u8], create_dirs: bool) -> Result<()> {
        if create_dirs {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                io("mkdir", parent, std::fs::create_dir_all(parent))?;
            }
        }
        io("write", path, std::fs::write(path, data))
    }

    fn append(&self, path: &Path, data: &[u8]) -> Result<()> {
        use std::io::Write;
        let mut f = io(
            "open",
            path,
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path),
        )?;
        io("append", path, f.write_all(data))
    }

    fn list(&self, path: &Path) -> Result<Vec<Entry>> {
        let rd = io("list", path, std::fs::read_dir(path))?;
        let mut out = Vec::new();
        for e in rd {
            let e = io("list", path, e)?;
            let p = e.path();
            let md = io("stat", &p, std::fs::symlink_metadata(&p))?;
            out.push(entry_from(&p, &md));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn stat(&self, path: &Path) -> Result<Entry> {
        let md = io("stat", path, std::fs::symlink_metadata(path))?;
        Ok(entry_from(path, &md))
    }

    fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        if recursive {
            io("mkdir", path, std::fs::create_dir_all(path))
        } else {
            io("mkdir", path, std::fs::create_dir(path))
        }
    }

    fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        let md = io("stat", path, std::fs::symlink_metadata(path))?;
        if md.file_type().is_dir() && !md.file_type().is_symlink() {
            if recursive {
                io("remove", path, std::fs::remove_dir_all(path))
            } else {
                io("remove", path, std::fs::remove_dir(path))
            }
        } else {
            io("remove", path, std::fs::remove_file(path))
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        match std::fs::rename(from, to) {
            Ok(()) => Ok(()),
            // Across filesystems `rename` fails with EXDEV; move a file by
            // copy + delete instead (directories are not moved that way).
            Err(e)
                if e.raw_os_error() == Some(EXDEV)
                    && std::fs::symlink_metadata(from).is_ok_and(|m| m.is_file()) =>
            {
                io("copy", from, std::fs::copy(from, to))?;
                io("remove", from, std::fs::remove_file(from))
            }
            Err(e) => io("rename", from, Err(e)),
        }
    }

    fn copy(&self, from: &Path, to: &Path) -> Result<u64> {
        let md = io("stat", from, std::fs::metadata(from))?;
        if md.is_dir() {
            return Err(Error::invalid(format!(
                "copy {}: is a directory",
                from.display()
            )));
        }
        // Copying a file onto itself truncates it to zero bytes.
        if let (Ok(a), Ok(b)) = (std::fs::canonicalize(from), std::fs::canonicalize(to)) {
            if a == b {
                return Err(Error::invalid(format!(
                    "copy {}: source and destination are the same file",
                    from.display()
                )));
            }
        }
        io("copy", from, std::fs::copy(from, to))
    }

    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }
}
