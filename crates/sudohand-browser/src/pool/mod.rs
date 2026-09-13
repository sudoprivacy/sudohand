//! Durable job records shared with the ai-dev-browser pool state format.
//! The worker scheduler builds on these records; persistence does not execute jobs.

mod model;
mod profile;
mod state;

pub use model::{Job, JobResult, JobStatus};
pub use profile::{ProfileManager, ProfileMode};
pub use state::{load_state, save_state, PoolState};
