# Browser worker pool

The Rust SDK exposes `sudohand_browser::pool::BrowserPool`. This is the
application-level counterpart of ai-dev-browser's Python pool; it does not
provide Python imports or run arbitrary Python task names.

## Client ownership

Implement `PoolClient` and pass a `ClientFactory` to `BrowserPool::start`.
Each factory invocation receives a unique worker id and port, headless mode,
the selected cookie path and application-specific `client_kwargs`.

The factory creates and connects its Chrome, loads cookies when appropriate,
and returns a client that owns the browser lifetime. `execute` receives the
job's task type, positional arguments, keyword arguments and `JobContext`.
Dispatch task names inside that implementation. `close` must save cookies,
honor `close_browser`, and finish cleanup before returning. Use a process guard
so factory errors also clean up browsers that were already launched.

[The real-browser integration example](../crates/sudohand-browser/tests/pool_browser.rs)
shows a complete factory and client. It runs two isolated Chromes concurrently,
saves each worker's cookies, restores them into new browsers, and verifies that
shutdown releases the debugging ports.

## Submission and results

- `submit` queues one job; `submit_batch` queues several.
- `run` additionally accepts a retry budget and `hold`. Held jobs start when
  `wait` releases them, allowing a batch to receive one shared progress target.
- `wait_for` returns one terminal `JobResult`. `wait` returns a map keyed by
  job id, optionally stopping when selected jobs reach `min_success`.
- A wait timeout leaves the job running. It does not cancel or resubmit it.
- `ExecutionResult::from(value)` reports successful execution. Return
  `ExecutionResult { data, success: false }` for a completed business failure;
  return `Err(JobFailure)` for an execution failure eligible for retry.
- A finite `max_retries` follows the reference's attempt budget: `2` allows
  two failed attempts in total. `0` still permits the initial attempt, and `-1`
  retries without a fixed limit. `RequeuePosition` selects priority-front or
  normal-queue-back retry placement.

Use `get_status`, `workers`, `worker_stats` and `get_result` for snapshots.
Execution statistics count a returned business failure as completed execution,
while the job result's `success` remains false, matching the reference.

## Progress, persistence and shutdown

For a shared target, call `context.progress(current_success)` with the current
total for that job, rather than a delta. Stop collecting when it returns false.
`context.min_success()` exposes the explicit job override or shared target.
Keep a `context.selection()` guard while waiting for an interactive selection;
the pool does not expire its wait while a selection guard is active.

`ProfileMode` selects shared, per-worker or temporary cookie storage. A pool
`state_file` stores pending, interrupted and completed jobs atomically. Starting
another pool with that file resumes unfinished jobs and retains completed
results. Cookie files and job checkpoints are separate resources.

`add_worker` scales up; `remove_worker(id, true)` finishes that worker's current
job before closing it. Passing false cancels that invocation and requeues it.
`shutdown(true)` finishes current jobs but does not drain the pending queue;
call `wait` first when every submitted job must complete. `shutdown(false)`
preserves interrupted jobs for recovery. An interrupted shutdown/removal can
be awaited again: unfinished worker handles remain owned by the pool.

Always await `shutdown` before ending the Tokio runtime. Dropping the pool
signals cancellation, but a synchronous destructor cannot await browser cleanup.
