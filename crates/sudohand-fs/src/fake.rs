//! An in-memory tree that records every mutation. Used by consumers to
//! test their policy logic (and by this crate's own tests) without
//! touching the disk. Its error categories match [`RealFs`](crate::RealFs)
//! (verified by `tests/differential.rs`):
//! - an ancestor that is a file → `io` (ENOTDIR); a missing ancestor →
//!   `not_found`
//! - writing / appending / copying onto a directory → `io`
//! - `mkdir` over anything that exists → `io` (with `recursive`, an existing
//!   directory is fine)
//! - `rename` of a file onto a directory, a directory onto a file, onto a
//!   non-empty directory, or into its own subtree → `io`

use crate::backend::{Entry, EntryKind, FsBackend};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use sudohand_core::{Error, Result};

#[derive(Debug, Clone)]
enum Node {
    File(Vec<u8>),
    Dir,
}

type Nodes = BTreeMap<PathBuf, Node>;

#[derive(Debug, Default)]
pub struct FakeFs {
    nodes: Mutex<Nodes>,
    pub actions: Mutex<Vec<String>>,
}

fn missing(p: &Path) -> Error {
    Error::not_found(format!("{}: no such file or directory", p.display()))
}
fn is_dir_err(p: &Path) -> Error {
    Error::io(format!("{}: is a directory", p.display()))
}
fn not_dir_err(p: &Path) -> Error {
    Error::io(format!("{}: not a directory", p.display()))
}

/// Every proper ancestor must be a directory (or absent).
fn check_ancestors(nodes: &Nodes, p: &Path) -> Result<()> {
    let mut cur = p.parent();
    while let Some(d) = cur {
        if d.as_os_str().is_empty() {
            break;
        }
        if let Some(Node::File(_)) = nodes.get(d) {
            return Err(not_dir_err(d));
        }
        cur = d.parent();
    }
    Ok(())
}

/// The immediate parent must exist as a directory.
fn check_parent(nodes: &Nodes, p: &Path) -> Result<()> {
    check_ancestors(nodes, p)?;
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => match nodes.get(d) {
            Some(Node::Dir) => Ok(()),
            Some(Node::File(_)) => Err(not_dir_err(d)),
            None => Err(missing(d)),
        },
        _ => Ok(()),
    }
}

fn create_parents(nodes: &mut Nodes, p: &Path) -> Result<()> {
    check_ancestors(nodes, p)?;
    let mut cur = p.parent();
    while let Some(d) = cur {
        if d.as_os_str().is_empty() {
            break;
        }
        nodes.entry(d.to_path_buf()).or_insert(Node::Dir);
        cur = d.parent();
    }
    Ok(())
}

fn has_children(nodes: &Nodes, p: &Path) -> bool {
    nodes.keys().any(|k| k.parent() == Some(p))
}

impl FakeFs {
    /// An empty tree containing only `/`.
    pub fn new() -> Arc<Self> {
        let fs = Self::default();
        fs.nodes
            .lock()
            .unwrap()
            .insert(PathBuf::from("/"), Node::Dir);
        Arc::new(fs)
    }
    /// Seed a file (creating parents).
    pub fn with_file(self: Arc<Self>, path: &str, data: &[u8]) -> Arc<Self> {
        let p = PathBuf::from(path);
        let mut nodes = self.nodes.lock().unwrap();
        create_parents(&mut nodes, &p).expect("seed path");
        nodes.insert(p, Node::File(data.to_vec()));
        drop(nodes);
        self
    }
    pub fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }
    fn log(&self, s: String) {
        self.actions.lock().unwrap().push(s);
    }
    fn nodes(&self) -> MutexGuard<'_, Nodes> {
        self.nodes.lock().unwrap()
    }
    fn entry(path: &Path, node: &Node) -> Entry {
        Entry {
            path: path.to_string_lossy().into_owned(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kind: match node {
                Node::File(_) => EntryKind::File,
                Node::Dir => EntryKind::Dir,
            },
            size: match node {
                Node::File(d) => d.len() as u64,
                Node::Dir => 0,
            },
            modified: Some(0),
        }
    }
    fn write_file(&self, path: &Path, data: &[u8], create_dirs: bool, append: bool) -> Result<()> {
        let mut nodes = self.nodes();
        if create_dirs {
            create_parents(&mut nodes, path)?;
        } else {
            check_parent(&nodes, path)?;
        }
        match nodes.get_mut(path) {
            Some(Node::Dir) => Err(is_dir_err(path)),
            Some(Node::File(d)) => {
                if append {
                    d.extend_from_slice(data);
                } else {
                    *d = data.to_vec();
                }
                Ok(())
            }
            None => {
                nodes.insert(path.to_path_buf(), Node::File(data.to_vec()));
                Ok(())
            }
        }
    }
}

impl FsBackend for FakeFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let nodes = self.nodes();
        check_ancestors(&nodes, path)?;
        match nodes.get(path) {
            Some(Node::File(d)) => Ok(d.clone()),
            Some(Node::Dir) => Err(is_dir_err(path)),
            None => Err(missing(path)),
        }
    }
    fn read_prefix(&self, path: &Path, max: usize) -> Result<Vec<u8>> {
        let mut d = self.read(path)?;
        d.truncate(max);
        Ok(d)
    }
    fn write(&self, path: &Path, data: &[u8], create_dirs: bool) -> Result<()> {
        self.log(format!("write {} {}B", path.display(), data.len()));
        self.write_file(path, data, create_dirs, false)
    }
    fn append(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.log(format!("append {} {}B", path.display(), data.len()));
        self.write_file(path, data, false, true)
    }
    fn list(&self, path: &Path) -> Result<Vec<Entry>> {
        let nodes = self.nodes();
        check_ancestors(&nodes, path)?;
        match nodes.get(path) {
            Some(Node::Dir) => {}
            Some(Node::File(_)) => return Err(not_dir_err(path)),
            None => return Err(missing(path)),
        }
        let mut out: Vec<Entry> = nodes
            .iter()
            .filter(|(p, _)| p.parent() == Some(path) && p.as_path() != path)
            .map(|(p, n)| Self::entry(p, n))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
    fn stat(&self, path: &Path) -> Result<Entry> {
        let nodes = self.nodes();
        check_ancestors(&nodes, path)?;
        nodes
            .get(path)
            .map(|n| Self::entry(path, n))
            .ok_or_else(|| missing(path))
    }
    fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        self.log(format!("mkdir {} recursive={recursive}", path.display()));
        let mut nodes = self.nodes();
        match nodes.get(path) {
            Some(Node::Dir) if recursive => return Ok(()),
            Some(_) => return Err(Error::io(format!("{}: already exists", path.display()))),
            None => {}
        }
        if recursive {
            create_parents(&mut nodes, path)?;
        } else {
            check_parent(&nodes, path)?;
        }
        nodes.insert(path.to_path_buf(), Node::Dir);
        Ok(())
    }
    fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        self.log(format!("remove {} recursive={recursive}", path.display()));
        let mut nodes = self.nodes();
        check_ancestors(&nodes, path)?;
        match nodes.get(path) {
            None => return Err(missing(path)),
            Some(Node::Dir) if !recursive && has_children(&nodes, path) => {
                return Err(Error::io(format!(
                    "{}: directory not empty",
                    path.display()
                )))
            }
            Some(_) => {}
        }
        nodes.retain(|p, _| !(p == path || p.starts_with(path)));
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.log(format!("rename {} -> {}", from.display(), to.display()));
        let mut nodes = self.nodes();
        check_ancestors(&nodes, from)?;
        let Some(src) = nodes.get(from).cloned() else {
            return Err(missing(from));
        };
        check_parent(&nodes, to)?;
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(Error::io(format!(
                "{}: cannot move a directory into itself",
                from.display()
            )));
        }
        match (&src, nodes.get(to)) {
            (Node::File(_), Some(Node::Dir)) => return Err(is_dir_err(to)),
            (Node::Dir, Some(Node::File(_))) => return Err(not_dir_err(to)),
            (Node::Dir, Some(Node::Dir)) => {
                if has_children(&nodes, to) {
                    return Err(Error::io(format!("{}: directory not empty", to.display())));
                }
                nodes.remove(to);
            }
            _ => {}
        }
        let moved: Vec<(PathBuf, Node)> = nodes
            .iter()
            .filter(|(p, _)| p.as_path() == from || p.starts_with(from))
            .map(|(p, n)| {
                let rel = p.strip_prefix(from).unwrap_or(Path::new(""));
                (to.join(rel), n.clone())
            })
            .collect();
        nodes.retain(|p, _| !(p == from || p.starts_with(from)));
        nodes.extend(moved);
        Ok(())
    }
    fn copy(&self, from: &Path, to: &Path) -> Result<u64> {
        self.log(format!("copy {} -> {}", from.display(), to.display()));
        let mut nodes = self.nodes();
        check_ancestors(&nodes, from)?;
        let data = match nodes.get(from) {
            Some(Node::File(d)) => d.clone(),
            Some(Node::Dir) => {
                return Err(Error::invalid(format!(
                    "copy {}: is a directory",
                    from.display()
                )))
            }
            None => return Err(missing(from)),
        };
        if from == to {
            return Err(Error::invalid(format!(
                "copy {}: source and destination are the same file",
                from.display()
            )));
        }
        check_parent(&nodes, to)?;
        if matches!(nodes.get(to), Some(Node::Dir)) {
            return Err(is_dir_err(to));
        }
        let n = data.len() as u64;
        nodes.insert(to.to_path_buf(), Node::File(data));
        Ok(n)
    }
    fn exists(&self, path: &Path) -> bool {
        let nodes = self.nodes();
        check_ancestors(&nodes, path).is_ok() && nodes.contains_key(path)
    }
}
