//! Background worker pool for Application Layer orchestration (T-008).
//!
//! The pool is intentionally independent from Tauri and from any concrete
//! scanner/index implementation. Use cases enqueue small cooperative steps and
//! check [`CancellationToken`] between steps.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

type JobFn = Box<dyn FnOnce(CancellationToken) + Send + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobPriority {
    Background = 0,
    Normal = 1,
    Interactive = 2,
    Urgent = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobOutcome {
    Completed,
    CancelledBeforeStart,
    Panicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(u64);

impl JobId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct JobHandle {
    id: JobId,
    cancellation: CancellationToken,
    completion: Arc<JobCompletion>,
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub fn wait(&self) -> JobOutcome {
        self.completion.wait()
    }
}

#[derive(Debug, Default)]
struct JobCompletion {
    outcome: Mutex<Option<JobOutcome>>,
    changed: Condvar,
}

impl JobCompletion {
    fn complete(&self, outcome: JobOutcome) {
        let mut guard = self.outcome.lock().expect("job completion mutex poisoned");
        *guard = Some(outcome);
        self.changed.notify_all();
    }

    fn wait(&self) -> JobOutcome {
        let mut guard = self.outcome.lock().expect("job completion mutex poisoned");
        loop {
            if let Some(outcome) = *guard {
                return outcome;
            }
            guard = self
                .changed
                .wait(guard)
                .expect("job completion mutex poisoned");
        }
    }
}

struct QueuedJob {
    priority: JobPriority,
    sequence: u64,
    cancellation: CancellationToken,
    completion: Arc<JobCompletion>,
    job: JobFn,
}

impl Eq for QueuedJob {}

impl PartialEq for QueuedJob {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.sequence == other.sequence
    }
}

impl Ord for QueuedJob {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for QueuedJob {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
struct QueueState {
    accepting: bool,
    next_sequence: u64,
    jobs: BinaryHeap<QueuedJob>,
}

struct SharedQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

impl Default for SharedQueue {
    fn default() -> Self {
        Self {
            state: Mutex::new(QueueState {
                accepting: true,
                next_sequence: 0,
                jobs: BinaryHeap::new(),
            }),
            changed: Condvar::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerPoolConfig {
    pub workers: usize,
}

impl Default for WorkerPoolConfig {
    fn default() -> Self {
        Self { workers: 1 }
    }
}

pub struct WorkerPool {
    queue: Arc<SharedQueue>,
    next_id: AtomicU64,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    pub fn new(config: WorkerPoolConfig) -> Self {
        let worker_count = config.workers.max(1);
        let queue = Arc::new(SharedQueue::default());
        let mut workers = Vec::with_capacity(worker_count);

        for _ in 0..worker_count {
            let worker_queue = Arc::clone(&queue);
            workers.push(thread::spawn(move || worker_loop(worker_queue)));
        }

        Self {
            queue,
            next_id: AtomicU64::new(1),
            workers,
        }
    }

    pub fn submit<F>(&self, priority: JobPriority, job: F) -> JobHandle
    where
        F: FnOnce(CancellationToken) + Send + 'static,
    {
        let id = JobId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let cancellation = CancellationToken::new();
        let completion = Arc::new(JobCompletion::default());

        let mut state = self
            .queue
            .state
            .lock()
            .expect("worker queue mutex poisoned");
        assert!(state.accepting, "worker pool is shutting down");

        let sequence = state.next_sequence;
        state.next_sequence = state
            .next_sequence
            .checked_add(1)
            .expect("worker queue sequence exhausted");
        state.jobs.push(QueuedJob {
            priority,
            sequence,
            cancellation: cancellation.clone(),
            completion: Arc::clone(&completion),
            job: Box::new(job),
        });
        drop(state);
        self.queue.changed.notify_one();

        JobHandle {
            id,
            cancellation,
            completion,
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        {
            let mut state = self
                .queue
                .state
                .lock()
                .expect("worker queue mutex poisoned");
            state.accepting = false;
        }
        self.queue.changed.notify_all();

        for worker in self.workers.drain(..) {
            worker.join().expect("worker thread panicked");
        }
    }
}

fn worker_loop(queue: Arc<SharedQueue>) {
    loop {
        let queued = {
            let mut state = queue.state.lock().expect("worker queue mutex poisoned");
            loop {
                if let Some(job) = state.jobs.pop() {
                    break job;
                }
                if !state.accepting {
                    return;
                }
                state = queue
                    .changed
                    .wait(state)
                    .expect("worker queue mutex poisoned");
            }
        };

        if queued.cancellation.is_cancelled() {
            queued.completion.complete(JobOutcome::CancelledBeforeStart);
            continue;
        }

        let token = queued.cancellation.clone();
        let outcome = match panic::catch_unwind(AssertUnwindSafe(|| (queued.job)(token))) {
            Ok(()) => JobOutcome::Completed,
            Err(_) => JobOutcome::Panicked,
        };
        queued.completion.complete(outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn higher_priority_job_runs_before_background_queue() {
        let pool = WorkerPool::new(WorkerPoolConfig { workers: 1 });
        let (release_running, wait_running) = mpsc::channel();
        let (running_started, wait_started) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));

        let running_events = Arc::clone(&events);
        let running = pool.submit(JobPriority::Normal, move |_| {
            running_events
                .lock()
                .expect("events mutex poisoned")
                .push("running");
            running_started.send(()).expect("signal running job start");
            wait_running.recv().expect("release running job");
        });

        wait_started.recv().expect("running job should start first");

        let background_events = Arc::clone(&events);
        let background = pool.submit(JobPriority::Background, move |_| {
            background_events
                .lock()
                .expect("events mutex poisoned")
                .push("background");
        });

        let urgent_events = Arc::clone(&events);
        let urgent = pool.submit(JobPriority::Urgent, move |_| {
            urgent_events
                .lock()
                .expect("events mutex poisoned")
                .push("urgent");
        });

        release_running.send(()).expect("release running job");
        assert_eq!(running.wait(), JobOutcome::Completed);
        assert_eq!(urgent.wait(), JobOutcome::Completed);
        assert_eq!(background.wait(), JobOutcome::Completed);
        assert_eq!(
            *events.lock().expect("events mutex poisoned"),
            vec!["running", "urgent", "background"]
        );
    }

    #[test]
    fn queued_job_can_be_cancelled_before_start() {
        let pool = WorkerPool::new(WorkerPoolConfig { workers: 1 });
        let (release_running, wait_running) = mpsc::channel();
        let ran = Arc::new(AtomicBool::new(false));

        let running = pool.submit(JobPriority::Normal, move |_| {
            wait_running.recv().expect("release running job");
        });

        let queued_ran = Arc::clone(&ran);
        let queued = pool.submit(JobPriority::Normal, move |_| {
            queued_ran.store(true, Ordering::Release);
        });

        queued.cancel();
        release_running.send(()).expect("release running job");
        assert_eq!(running.wait(), JobOutcome::Completed);
        assert_eq!(queued.wait(), JobOutcome::CancelledBeforeStart);
        assert!(!ran.load(Ordering::Acquire));
    }

    #[test]
    fn running_job_observes_cancellation_between_steps() {
        let pool = WorkerPool::new(WorkerPoolConfig { workers: 1 });
        let (step_started, wait_continue) = mpsc::channel();
        let (allow_continue, continue_signal) = mpsc::channel();
        let steps = Arc::new(AtomicU64::new(0));

        let job_steps = Arc::clone(&steps);
        let job = pool.submit(JobPriority::Normal, move |token| {
            job_steps.fetch_add(1, Ordering::AcqRel);
            step_started.send(()).expect("signal first step");
            continue_signal.recv().expect("continue job");
            if token.is_cancelled() {
                return;
            }
            job_steps.fetch_add(1, Ordering::AcqRel);
        });

        wait_continue.recv().expect("first step should start");
        job.cancel();
        allow_continue.send(()).expect("allow job to continue");
        assert_eq!(job.wait(), JobOutcome::Completed);
        assert_eq!(steps.load(Ordering::Acquire), 1);
    }
}
