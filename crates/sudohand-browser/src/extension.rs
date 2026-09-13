//! Bundled extension assets, extracted for Chrome's Load unpacked workflow.
use crate::Result;
use std::path::PathBuf;

/// Extract the standalone extension shipped inside the Rust binary.
pub fn extension_dir() -> Result<PathBuf> {
    let directory = crate::config::base_dir()
        .join("suh-extension")
        .join("0.1.0");
    std::fs::create_dir_all(&directory)?;
    for (name, content) in [
        ("manifest.json", include_str!("../extension/manifest.json")),
        ("background.js", include_str!("../extension/background.js")),
    ] {
        let path = directory.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
            std::fs::write(path, content)?;
        }
    }
    Ok(directory)
}
