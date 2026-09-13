//! Concurrent browser worker pool with durable jobs and cooperative progress.
//! Implement [`PoolClient`] and supply a [`ClientFactory`] for browser lifecycle
//! and application tasks. [`BrowserPool`] handles queues, retries, waits, scaling,
//! cancellation, and checkpoints compatible with ai-dev-browser's state format.

mod model;
mod profile;
mod scheduler;
mod state;
mod worker;

pub use model::{Job, JobResult, JobStatus};
pub use profile::{ProfileManager, ProfileMode};
pub use scheduler::{
    BrowserPool, ClientFactory, ExecutionResult, FailCondition, JobContext, JobFailure, PoolClient,
    PoolFuture, PoolOptions, RequeuePosition, SelectionGuard, WorkerOptions,
};
pub use state::{load_state, save_state, PoolState};
pub use worker::{Worker, WorkerStats, WorkerStatus};
