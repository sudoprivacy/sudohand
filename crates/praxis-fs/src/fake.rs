//! An in-memory tree that records every mutation. Used by consumers to
//! test their policy logic (and by this crate's own tests) without
//! touching the disk.

use crate::backend::{Entry, EntryKind, FsBackend};
use praxis_core::{Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
enum Node {
    File(Vec<u8>),
    Dir,
}

#[derive(Debug, Default)]
pub struct FakeFs {
    nodes: Mutex<BTreeMap<PathBuf, Node>>,
    pub actions: Mutex<Vec<String>>,
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
        self.ensure_parents(&p);
        self.nodes
            .lock()
            .unwrap()
            .insert(p, Node::File(data.to_vec()));
        self
    }
    pub fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }
    fn log(&self, s: String) {
        self.actions.lock().unwrap().push(s);
    }
    fn ensure_parents(&self, p: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let mut cur = p.parent();
        while let Some(d) = cur {
            nodes.entry(d.to_path_buf()).or_insert(Node::Dir);
            cur = d.parent();
        }
    }
    fn entry(path: &Path, node: &Node) -> Entry {
        Entry {
            path: path.to_path_buf(),
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
    fn missing(p: &Path) -> Error {
        Error::not_found(format!("{}: no such file or directory", p.display()))
    }
}

impl FsBackend for FakeFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        match self.nodes.lock().unwrap().get(path) {
            Some(Node::File(d)) => Ok(d.clone()),
            Some(Node::Dir) => Err(Error::io(format!("{}: is a directory", path.display()))),
            None => Err(Self::missing(path)),
        }
    }
    fn write(&self, path: &Path, data: &[u8], create_dirs: bool) -> Result<()> {
        self.log(format!("write {} {}B", path.display(), data.len()));
        if create_dirs {
            self.ensure_parents(path);
        } else if let Some(parent) = path.parent() {
            if !matches!(self.nodes.lock().unwrap().get(parent), Some(Node::Dir)) {
                return Err(Self::missing(parent));
            }
        }
        self.nodes
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), Node::File(data.to_vec()));
        Ok(())
    }
    fn append(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.log(format!("append {} {}B", path.display(), data.len()));
        let mut nodes = self.nodes.lock().unwrap();
        match nodes
            .entry(path.to_path_buf())
            .or_insert(Node::File(Vec::new()))
        {
            Node::File(d) => {
                d.extend_from_slice(data);
                Ok(())
            }
            Node::Dir => Err(Error::io(format!("{}: is a directory", path.display()))),
        }
    }
    fn list(&self, path: &Path) -> Result<Vec<Entry>> {
        let nodes = self.nodes.lock().unwrap();
        match nodes.get(path) {
            Some(Node::Dir) => {}
            Some(Node::File(_)) => {
                return Err(Error::io(format!("{}: not a directory", path.display())))
            }
            None => return Err(Self::missing(path)),
        }
        Ok(nodes
            .iter()
            .filter(|(p, _)| p.parent() == Some(path) && p.as_path() != path)
            .map(|(p, n)| Self::entry(p, n))
            .collect())
    }
    fn stat(&self, path: &Path) -> Result<Entry> {
        self.nodes
            .lock()
            .unwrap()
            .get(path)
            .map(|n| Self::entry(path, n))
            .ok_or_else(|| Self::missing(path))
    }
    fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        self.log(format!("mkdir {} recursive={recursive}", path.display()));
        if recursive {
            self.ensure_parents(path);
        } else {
            if self.nodes.lock().unwrap().contains_key(path) {
                return Err(Error::io(format!("{}: already exists", path.display())));
            }
            if let Some(parent) = path.parent() {
                if !matches!(self.nodes.lock().unwrap().get(parent), Some(Node::Dir)) {
                    return Err(Self::missing(parent));
                }
            }
        }
        self.nodes
            .lock()
            .unwrap()
            .entry(path.to_path_buf())
            .or_insert(Node::Dir);
        Ok(())
    }
    fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        self.log(format!("remove {} recursive={recursive}", path.display()));
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(path) {
            None => return Err(Self::missing(path)),
            Some(Node::Dir) => {
                let has_children = nodes.keys().any(|p| p.parent() == Some(path));
                if has_children && !recursive {
                    return Err(Error::io(format!(
                        "{}: directory not empty",
                        path.display()
                    )));
                }
            }
            Some(Node::File(_)) => {}
        }
        nodes.retain(|p, _| !(p == path || p.starts_with(path)));
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.log(format!("rename {} -> {}", from.display(), to.display()));
        let mut nodes = self.nodes.lock().unwrap();
        if !nodes.contains_key(from) {
            return Err(Self::missing(from));
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
        let data = match self.nodes.lock().unwrap().get(from) {
            Some(Node::File(d)) => d.clone(),
            Some(Node::Dir) => {
                return Err(Error::invalid(format!(
                    "copy {}: is a directory",
                    from.display()
                )))
            }
            None => return Err(Self::missing(from)),
        };
        let n = data.len() as u64;
        self.nodes
            .lock()
            .unwrap()
            .insert(to.to_path_buf(), Node::File(data));
        Ok(n)
    }
    fn exists(&self, path: &Path) -> bool {
        self.nodes.lock().unwrap().contains_key(path)
    }
}
