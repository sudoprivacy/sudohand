//! The fake and the real backend must agree on behaviour; run the same
//! scenario against both.

use praxis_fs::{EntryKind, FakeFs, FsBackend, RealFs};
use std::path::{Path, PathBuf};

fn scenario(fs: &dyn FsBackend, root: &Path) {
    let a = root.join("a.txt");
    let sub = root.join("sub");
    let nested = sub.join("deep").join("b.bin");

    assert!(!fs.exists(&a));
    assert_eq!(fs.read(&a).unwrap_err().code(), "not_found");

    fs.write(&a, b"hello", false).unwrap();
    assert_eq!(fs.read(&a).unwrap(), b"hello");
    fs.append(&a, b" world").unwrap();
    assert_eq!(fs.read(&a).unwrap(), b"hello world");

    // create_dirs=false into a missing dir fails; true creates it.
    assert_eq!(
        fs.write(&nested, b"\x00\xff", false).unwrap_err().code(),
        "not_found"
    );
    fs.write(&nested, b"\x00\xff", true).unwrap();
    assert_eq!(fs.read(&nested).unwrap(), b"\x00\xff");

    let st = fs.stat(&a).unwrap();
    assert_eq!(
        (st.kind, st.size, st.name.as_str()),
        (EntryKind::File, 11, "a.txt")
    );
    assert_eq!(fs.stat(&sub).unwrap().kind, EntryKind::Dir);

    let names: Vec<String> = fs.list(root).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["a.txt", "sub"]);
    assert_eq!(fs.list(&a).unwrap_err().code(), "io");

    let c = root.join("c.txt");
    assert_eq!(fs.copy(&a, &c).unwrap(), 11);
    assert_eq!(fs.copy(&sub, &c).unwrap_err().code(), "invalid_input");
    let d = root.join("d.txt");
    fs.rename(&c, &d).unwrap();
    assert!(!fs.exists(&c) && fs.exists(&d));

    let e = root.join("e");
    fs.mkdir(&e, false).unwrap();
    assert_eq!(fs.mkdir(&e, false).unwrap_err().code(), "io");
    fs.mkdir(&e, true).unwrap();

    assert_eq!(fs.remove(&sub, false).unwrap_err().code(), "io");
    fs.remove(&sub, true).unwrap();
    assert!(!fs.exists(&sub));
    fs.remove(&a, false).unwrap();
    assert_eq!(fs.remove(&a, false).unwrap_err().code(), "not_found");
}

#[test]
fn fake_backend() {
    let fs = FakeFs::new();
    fs.mkdir(Path::new("/w"), true).unwrap();
    scenario(&*fs, Path::new("/w"));
    let log = fs.actions();
    assert!(log.iter().any(|l| l == "write /w/a.txt 5B"));
    assert!(log.iter().any(|l| l == "remove /w/sub recursive=true"));
}

#[test]
fn real_backend() {
    let root: PathBuf = std::env::temp_dir().join(format!("praxis-fs-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    scenario(&RealFs::new(), &root);
    std::fs::remove_dir_all(&root).unwrap();
}
