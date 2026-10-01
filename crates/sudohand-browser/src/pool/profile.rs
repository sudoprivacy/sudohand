use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{config, Result};

/// Workers always have separate Chrome profiles. This selects whether their
/// clients load/save one shared cookie file, one file per worker, or no file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    #[default]
    Shared,
    PerWorker,
    Temp,
}

#[derive(Clone, Debug)]
pub struct ProfileManager {
    pub mode: ProfileMode,
    pub cookies_file: PathBuf,
    pub cookies_dir: PathBuf,
}

fn expand(path: PathBuf) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        config::home_dir().join(rest)
    } else {
        path
    }
}

impl ProfileManager {
    pub fn new(
        mode: ProfileMode,
        cookies_file: Option<PathBuf>,
        cookies_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let manager = Self {
            mode,
            cookies_file: expand(
                cookies_file.unwrap_or_else(|| config::base_dir().join("cookies.dat")),
            ),
            cookies_dir: expand(cookies_dir.unwrap_or_else(|| config::base_dir().join("cookies"))),
        };
        match mode {
            ProfileMode::Shared => {
                let parent = manager
                    .cookies_file
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                std::fs::create_dir_all(parent)?;
            }
            ProfileMode::PerWorker => std::fs::create_dir_all(&manager.cookies_dir)?,
            ProfileMode::Temp => {}
        }
        Ok(manager)
    }

    /// A path is returned before the file exists, so a first login can save it.
    /// Persistence itself belongs to the worker's client lifecycle.
    #[must_use]
    pub fn get_cookies_file(&self, worker_id: u64) -> Option<PathBuf> {
        match self.mode {
            ProfileMode::Shared => Some(self.cookies_file.clone()),
            ProfileMode::PerWorker => Some(
                self.cookies_dir
                    .join(format!("cookies_worker_{worker_id}.dat")),
            ),
            ProfileMode::Temp => None,
        }
    }
}
