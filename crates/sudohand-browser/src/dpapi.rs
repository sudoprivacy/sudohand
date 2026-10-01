//! Windows user-scope DPAPI through the platform's managed crypto API.
//! Data travels over stdin/stdout, never command-line arguments or logs.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

use crate::{Error, Result};

pub fn unprotect(encrypted: &[u8]) -> Result<Vec<u8>> {
    let program = r"$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.Security; $data = [Convert]::FromBase64String([Console]::In.ReadToEnd().Trim()); $plain = [Security.Cryptography.ProtectedData]::Unprotect($data, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser); [Console]::Out.Write([Convert]::ToBase64String($plain))";
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            program,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Error::Invalid(format!("Windows DPAPI helper could not start: {error}"))
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(
            base64::engine::general_purpose::STANDARD
                .encode(encrypted)
                .as_bytes(),
        )?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(Error::Invalid(
            "Windows DPAPI could not decrypt data for the current user".into(),
        ));
    }
    base64::engine::general_purpose::STANDARD
        .decode(String::from_utf8_lossy(&output.stdout).trim())
        .map_err(|_| Error::Invalid("Windows DPAPI returned invalid data".into()))
}
