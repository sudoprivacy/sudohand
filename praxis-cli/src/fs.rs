//! `praxis fs <command> [flags]` — thin CLI over `praxis-fs`.

use clap::Subcommand;
use praxis_fs::{Error, FsBackend, Result};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Subcommand)]
pub enum Cmd {
    /// File contents: `text` when valid UTF-8 (or `base64` otherwise / with --base64).
    Read {
        #[arg(long)]
        path: String,
        #[arg(long)]
        base64: bool,
        /// Return at most this many bytes (sets `truncated`).
        #[arg(long)]
        max_bytes: Option<usize>,
    },
    /// Create/overwrite a file from --text, --base64 or stdin.
    Write {
        #[arg(long)]
        path: String,
        #[arg(long, conflicts_with_all = ["base64", "stdin"])]
        text: Option<String>,
        #[arg(long, conflicts_with = "stdin")]
        base64: Option<String>,
        /// Read the content from stdin.
        #[arg(long)]
        stdin: bool,
        /// Append instead of overwrite.
        #[arg(long)]
        append: bool,
        /// Create missing parent directories.
        #[arg(long)]
        create_dirs: bool,
    },
    /// Directory entries (name, kind, size, modified).
    Ls {
        #[arg(long)]
        path: String,
    },
    /// Metadata of one path (symlinks not followed).
    Stat {
        #[arg(long)]
        path: String,
    },
    /// Create a directory (`-p` for parents / idempotent).
    Mkdir {
        #[arg(long)]
        path: String,
        #[arg(short = 'p', long)]
        parents: bool,
    },
    /// Delete a file or directory (`-r` for a non-empty tree).
    Rm {
        #[arg(long)]
        path: String,
        #[arg(short, long)]
        recursive: bool,
    },
    /// Rename / move.
    Mv {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    /// Copy one file.
    Cp {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    /// Whether anything exists at the path.
    Exists {
        #[arg(long)]
        path: String,
    },
}

pub fn run(cmd: Cmd) -> Result<Value> {
    run_with(&praxis_fs::RealFs::new(), cmd)
}

pub fn run_with(fs: &dyn FsBackend, cmd: Cmd) -> Result<Value> {
    Ok(match cmd {
        Cmd::Read {
            path,
            base64,
            max_bytes,
        } => {
            let data = fs.read(Path::new(&path))?;
            let total = data.len();
            let (slice, truncated) = match max_bytes {
                Some(n) if n < total => (&data[..n], true),
                _ => (&data[..], false),
            };
            let mut v = json!({"path": path, "bytes": total, "truncated": truncated});
            match (base64, std::str::from_utf8(slice)) {
                (false, Ok(s)) => v["text"] = json!(s),
                _ => v["base64"] = json!(praxis_core::b64::encode(slice)),
            }
            v
        }
        Cmd::Write {
            path,
            text,
            base64,
            stdin,
            append,
            create_dirs,
        } => {
            let data: Vec<u8> = if let Some(t) = text {
                t.into_bytes()
            } else if let Some(b) = base64 {
                decode_b64(&b)?
            } else if stdin {
                use std::io::Read;
                let mut buf = Vec::new();
                std::io::stdin()
                    .read_to_end(&mut buf)
                    .map_err(|e| Error::io(format!("read stdin: {e}")))?;
                buf
            } else {
                return Err(Error::invalid("write needs --text, --base64 or --stdin"));
            };
            let p = Path::new(&path);
            if append {
                fs.append(p, &data)?;
            } else {
                fs.write(p, &data, create_dirs)?;
            }
            json!({"path": path, "bytes": data.len(), "appended": append})
        }
        Cmd::Ls { path } => json!({"path": path, "entries": fs.list(Path::new(&path))?}),
        Cmd::Stat { path } => json!(fs.stat(Path::new(&path))?),
        Cmd::Mkdir { path, parents } => {
            fs.mkdir(Path::new(&path), parents)?;
            json!({"created": path})
        }
        Cmd::Rm { path, recursive } => {
            fs.remove(Path::new(&path), recursive)?;
            json!({"removed": path})
        }
        Cmd::Mv { from, to } => {
            fs.rename(Path::new(&from), Path::new(&to))?;
            json!({"moved": {"from": from, "to": to}})
        }
        Cmd::Cp { from, to } => {
            let n = fs.copy(Path::new(&from), Path::new(&to))?;
            json!({"copied": {"from": from, "to": to, "bytes": n}})
        }
        Cmd::Exists { path } => json!({"path": path, "exists": fs.exists(Path::new(&path))}),
    })
}

/// Standard base64 (RFC 4648, `=` padding) decoder; the counterpart of
/// `praxis_core::b64::encode`.
fn decode_b64(s: &str) -> Result<Vec<u8>> {
    let val = |c: u8| -> Result<u32> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(Error::invalid(format!("invalid base64 byte {c:?}"))),
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let trimmed = bytes.iter().rposition(|&b| b != b'=').map_or(0, |i| i + 1);
    if !bytes.len().is_multiple_of(4) || bytes.len() - trimmed > 2 {
        return Err(Error::invalid("invalid base64 length/padding"));
    }
    let mut out = Vec::with_capacity(trimmed * 3 / 4);
    for chunk in bytes[..trimmed].chunks(4) {
        let mut n = 0u32;
        for &c in chunk {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * (4 - chunk.len() as u32);
        let full = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&full[..chunk.len() - 1]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxis_fs::FakeFs;

    #[test]
    fn b64_roundtrip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let enc = praxis_core::b64::encode(s.as_bytes());
            assert_eq!(decode_b64(&enc).unwrap(), s.as_bytes());
        }
        assert_eq!(decode_b64("////").unwrap(), [0xff, 0xff, 0xff]);
        assert!(decode_b64("abc").is_err());
        assert!(decode_b64("ab!=").is_err());
    }

    #[test]
    fn json_shapes() {
        let fs = FakeFs::new().with_file("/w/a.txt", b"hello");
        let v = run_with(
            &*fs,
            Cmd::Read {
                path: "/w/a.txt".into(),
                base64: false,
                max_bytes: Some(2),
            },
        )
        .unwrap();
        assert_eq!(v["text"], "he");
        assert_eq!(v["bytes"], 5);
        assert_eq!(v["truncated"], true);
        let v = run_with(
            &*fs,
            Cmd::Read {
                path: "/w/a.txt".into(),
                base64: true,
                max_bytes: None,
            },
        )
        .unwrap();
        assert_eq!(v["base64"], "aGVsbG8=");
        run_with(
            &*fs,
            Cmd::Write {
                path: "/w/b.bin".into(),
                text: None,
                base64: Some("AP8=".into()),
                stdin: false,
                append: false,
                create_dirs: false,
            },
        )
        .unwrap();
        let v = run_with(
            &*fs,
            Cmd::Read {
                path: "/w/b.bin".into(),
                base64: false,
                max_bytes: None,
            },
        )
        .unwrap();
        assert_eq!(v["base64"], "AP8=");
        let v = run_with(&*fs, Cmd::Ls { path: "/w".into() }).unwrap();
        assert_eq!(v["entries"][0]["name"], "a.txt");
        assert_eq!(v["entries"][0]["kind"], "file");
        let e = run_with(
            &*fs,
            Cmd::Stat {
                path: "/nope".into(),
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), "not_found");
        let e = run_with(
            &*fs,
            Cmd::Write {
                path: "/w/c".into(),
                text: None,
                base64: None,
                stdin: false,
                append: false,
                create_dirs: false,
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), "invalid_input");
    }
}
