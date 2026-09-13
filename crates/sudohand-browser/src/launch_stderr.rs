//! Drain Chrome stderr without blocking startup or retaining unbounded logs.
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

const TAIL_BYTES: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) struct LaunchStderr(Arc<Mutex<Vec<u8>>>);

impl LaunchStderr {
    pub(crate) fn start(mut reader: impl Read + Send + 'static) -> io::Result<Self> {
        let tail = Arc::new(Mutex::new(Vec::new()));
        let weak = Arc::downgrade(&tail);
        std::thread::Builder::new()
            .name("chrome-stderr".into())
            .spawn(move || {
                let mut chunk = [0; 8192];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => {
                            // After startup the owner drops the diagnostic buffer;
                            // keep draining until Chrome closes its stream. Neither
                            // a CLI exit nor an error path joins this reader thread.
                            if let Some(tail) = weak.upgrade() {
                                let mut tail = tail
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                tail.extend_from_slice(&chunk[..count]);
                                let discard = tail.len().saturating_sub(TAIL_BYTES);
                                tail.drain(..discard);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self(tail))
    }

    pub(crate) fn snapshot(&self) -> String {
        let tail = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&tail).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "subprocess fixture for the pipe regression"]
    fn noisy_child() {
        if std::env::var_os("SUH_STDERR_FIXTURE").is_none() {
            return;
        }
        let mut stderr = io::stderr().lock();
        stderr.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
        stderr.write_all(b"stderr-fixture-complete").unwrap();
        stderr.flush().unwrap();
    }

    #[test]
    fn drains_a_real_pipe_and_keeps_only_a_bounded_tail() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "launch_stderr::tests::noisy_child",
                "--ignored",
                "--nocapture",
            ])
            .env("SUH_STDERR_FIXTURE", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let capture = LaunchStderr::start(child.stderr.take().unwrap()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("stderr writer blocked before exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        loop {
            let tail = capture.snapshot();
            assert!(tail.len() <= TAIL_BYTES);
            if tail.ends_with("stderr-fixture-complete") {
                break;
            }
            assert!(Instant::now() < deadline, "missing stderr tail: {tail}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
