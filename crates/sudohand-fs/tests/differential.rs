//! Integrators test their policy against `FakeFs`; it must behave like
//! `RealFs`. Drive both with the same pseudo-random operation sequence and
//! require identical outcomes (Ok/Err category, contents, listings).

use std::path::{Path, PathBuf};
use sudohand_fs::{Entry, FakeFs, FsBackend, RealFs};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() as usize) % xs.len()]
    }
}

#[derive(Debug)]
enum Outcome {
    Ok(String),
    Err(&'static str),
}

fn norm(entries: Vec<Entry>, root: &Path) -> String {
    entries
        .into_iter()
        .map(|e| {
            let rel = Path::new(&e.path)
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(e.path);
            format!("{rel}:{:?}:{}", e.kind, e.size)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn apply(fs: &dyn FsBackend, root: &Path, op: &str, a: &Path, b: &Path, data: &[u8]) -> Outcome {
    let r: Result<String, sudohand_fs::Error> = match op {
        "write" => fs.write(a, data, false).map(|()| String::new()),
        "write_p" => fs.write(a, data, true).map(|()| String::new()),
        "append" => fs.append(a, data).map(|()| String::new()),
        "mkdir" => fs.mkdir(a, false).map(|()| String::new()),
        "mkdir_p" => fs.mkdir(a, true).map(|()| String::new()),
        "rm" => fs.remove(a, false).map(|()| String::new()),
        "rm_r" => fs.remove(a, true).map(|()| String::new()),
        "mv" => fs.rename(a, b).map(|()| String::new()),
        "cp" => fs.copy(a, b).map(|n| n.to_string()),
        "read" => fs.read(a).map(|d| String::from_utf8_lossy(&d).into_owned()),
        "prefix" => fs
            .read_prefix(a, 3)
            .map(|d| String::from_utf8_lossy(&d).into_owned()),
        "ls" => fs.list(a).map(|e| norm(e, root)),
        "stat" => fs.stat(a).map(|e| format!("{:?}:{}", e.kind, e.size)),
        "exists" => Ok(fs.exists(a).to_string()),
        _ => unreachable!(),
    };
    match r {
        Ok(s) => Outcome::Ok(s),
        Err(e) => Outcome::Err(e.code()),
    }
}

#[test]
fn fake_and_real_agree_on_random_sequences() {
    let ops = [
        "write", "write_p", "append", "mkdir", "mkdir_p", "rm", "rm_r", "mv", "cp", "read",
        "prefix", "ls", "stat", "exists",
    ];
    let names = ["a", "b", "d", "d/x", "d/y", "d/e", "d/e/z", "f", "nope/q"];
    let payloads: [&[u8]; 3] = [b"", b"hello", b"0123456789"];

    for seed in 1..=32u64 {
        let real_root: PathBuf =
            std::env::temp_dir().join(format!("sudohand-fs-diff-{}-{seed}", std::process::id()));
        let _ = std::fs::remove_dir_all(&real_root);
        std::fs::create_dir_all(&real_root).unwrap();
        let fake_root = PathBuf::from("/root");
        let fake = FakeFs::new();
        fake.mkdir(&fake_root, true).unwrap();
        let real = RealFs::new();
        let mut rng = Lcg(seed);

        for step in 0..800 {
            let op = *rng.pick(&ops);
            let (na, nb) = (*rng.pick(&names), *rng.pick(&names));
            let data = *rng.pick(&payloads);
            let got_fake = apply(
                &*fake,
                &fake_root,
                op,
                &fake_root.join(na),
                &fake_root.join(nb),
                data,
            );
            let got_real = apply(
                &real,
                &real_root,
                op,
                &real_root.join(na),
                &real_root.join(nb),
                data,
            );
            let same = match (&got_fake, &got_real) {
                (Outcome::Ok(x), Outcome::Ok(y)) => x == y,
                // The category must match; the OS wording may differ.
                (Outcome::Err(x), Outcome::Err(y)) => x == y,
                _ => false,
            };
            assert!(
                same,
                "seed {seed} step {step}: {op} {na} {nb} {data:?}\n fake: {got_fake:?}\n real: {got_real:?}"
            );
        }
        std::fs::remove_dir_all(&real_root).unwrap();
    }
}
