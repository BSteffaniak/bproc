//! Background process collector. Owns a `Sampler` on a dedicated thread and
//! publishes complete `ProcSnapshot`s; it never blocks the UI and the UI never
//! blocks it.
use super::model::{ExitedProcess, ProcSnapshot, ProcessKey, now_ms};
use super::platform::{RawSample, Sampler, create_sampler};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::error::TrySendError;

pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(1500);
pub const MIN_INTERVAL: Duration = Duration::from_millis(250);
/// Upper bound on the refresh interval (keeps `Instant` arithmetic sane).
pub const MAX_INTERVAL: Duration = Duration::from_secs(3600);
/// How often an undelivered snapshot is offered again while the UI is busy.
const RETRY_DELIVERY: Duration = Duration::from_millis(15);

#[derive(Debug)]
pub enum CollectorCommand {
    SetInterval(Duration),
    Pause(bool),
    RefreshNow,
    Shutdown,
}

pub struct CollectorHandle {
    pub commands: std_mpsc::Sender<CollectorCommand>,
    pub thread: std::thread::JoinHandle<()>,
}

impl CollectorHandle {
    /// Stop the collector and wait for it. Returns within one sample even if
    /// the receiver is alive but no longer draining snapshots.
    pub fn shutdown(self) {
        let _ = self.commands.send(CollectorCommand::Shutdown);
        let _ = self.thread.join();
    }
}

/// Spawn the collector. Snapshots are delivered on a bounded channel; when the
/// UI falls behind, newer snapshots replace the undelivered one (carrying its
/// spawned/exited events forward) instead of queueing stale data.
pub fn spawn(interval: Duration) -> (CollectorHandle, tokio::sync::mpsc::Receiver<ProcSnapshot>) {
    spawn_with(create_sampler(), interval)
}

pub fn spawn_with(
    sampler: Box<dyn Sampler>,
    interval: Duration,
) -> (CollectorHandle, tokio::sync::mpsc::Receiver<ProcSnapshot>) {
    let (cmd_tx, cmd_rx) = std_mpsc::channel();
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    let worker = Worker {
        sampler,
        commands: cmd_rx,
        tx,
        interval: clamp_interval(interval),
        paused: false,
        force: false,
        sequence: 0,
        previous: HashMap::new(),
        last_sample: None,
        next_due: Instant::now(),
        pending: None,
    };
    let thread = std::thread::Builder::new()
        .name("bproc-collector".into())
        .spawn(move || worker.run())
        .expect("spawn collector thread");
    (
        CollectorHandle {
            commands: cmd_tx,
            thread,
        },
        rx,
    )
}

/// Zero means "use the default"; anything else is kept within bounds.
fn clamp_interval(interval: Duration) -> Duration {
    if interval.is_zero() {
        DEFAULT_INTERVAL
    } else {
        interval.clamp(MIN_INTERVAL, MAX_INTERVAL)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

struct Worker {
    sampler: Box<dyn Sampler>,
    commands: std_mpsc::Receiver<CollectorCommand>,
    tx: tokio::sync::mpsc::Sender<ProcSnapshot>,
    interval: Duration,
    paused: bool,
    /// A `RefreshNow` is waiting to be served (also honoured while paused).
    force: bool,
    sequence: u64,
    previous: HashMap<ProcessKey, ExitedProcess>,
    last_sample: Option<Instant>,
    next_due: Instant,
    /// Snapshot the UI has not accepted yet.
    pending: Option<ProcSnapshot>,
}

impl Worker {
    fn run(mut self) {
        if self.prime() == Flow::Stop {
            return;
        }
        loop {
            if self.deliver() == Flow::Stop {
                return;
            }
            let now = Instant::now();
            if self.force || (!self.paused && now >= self.next_due) {
                self.force = false;
                self.tick();
                continue;
            }
            let mut wait = (!self.paused).then(|| self.next_due.saturating_duration_since(now));
            if self.pending.is_some() {
                wait = Some(wait.map_or(RETRY_DELIVERY, |w| w.min(RETRY_DELIVERY)));
            }
            let received = match wait {
                Some(wait) => self.commands.recv_timeout(wait),
                None => self
                    .commands
                    .recv()
                    .map_err(|_| std_mpsc::RecvTimeoutError::Disconnected),
            };
            let flow = match received {
                Ok(command) => self.handle(command),
                Err(std_mpsc::RecvTimeoutError::Timeout) => Flow::Continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => Flow::Stop,
            };
            if flow == Flow::Stop || self.drain_commands() == Flow::Stop {
                return;
            }
        }
    }

    /// Take a discarded warm-up sample so the first published CPU% and disk
    /// rates are real measurements, staying responsive to commands meanwhile.
    fn prime(&mut self) -> Flow {
        let Some(delay) = self.sampler.priming_delay() else {
            return Flow::Continue;
        };
        if let Err(error) = self.sampler.sample() {
            log::warn!("process sample failed: {error:#}");
            return Flow::Continue;
        }
        self.last_sample = Some(Instant::now());
        let deadline = Instant::now() + delay;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let flow = match self.commands.recv_timeout(left) {
                Ok(command) => self.handle(command),
                Err(std_mpsc::RecvTimeoutError::Timeout) => break,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => Flow::Stop,
            };
            if flow == Flow::Stop {
                return Flow::Stop;
            }
        }
        // The first real sample follows immediately regardless of pause.
        self.force = true;
        Flow::Continue
    }

    fn drain_commands(&mut self) -> Flow {
        loop {
            match self.commands.try_recv() {
                Ok(command) => {
                    if self.handle(command) == Flow::Stop {
                        return Flow::Stop;
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => return Flow::Continue,
                Err(std_mpsc::TryRecvError::Disconnected) => return Flow::Stop,
            }
        }
    }

    fn handle(&mut self, command: CollectorCommand) -> Flow {
        match command {
            CollectorCommand::Shutdown => return Flow::Stop,
            CollectorCommand::SetInterval(interval) => {
                self.interval = clamp_interval(interval);
                // Re-anchor on the last sample so a shorter interval that is
                // already overdue samples right away.
                self.next_due = self
                    .last_sample
                    .map_or_else(Instant::now, |t| t + self.interval);
            }
            CollectorCommand::Pause(paused) => {
                if self.paused && !paused {
                    self.next_due = Instant::now();
                }
                self.paused = paused;
            }
            CollectorCommand::RefreshNow => self.force = true,
        }
        Flow::Continue
    }

    fn tick(&mut self) {
        let raw = self.sampler.sample();
        let now = Instant::now();
        self.next_due = now + self.interval;
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                log::warn!("process sample failed: {error:#}");
                return;
            }
        };
        let interval_secs = self
            .last_sample
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(0.0);
        self.last_sample = Some(now);
        let snapshot = self.build(raw, interval_secs);
        self.pending = Some(match self.pending.take() {
            Some(stale) => coalesce(stale, snapshot),
            None => snapshot,
        });
    }

    fn build(&mut self, raw: RawSample, interval_secs: f32) -> ProcSnapshot {
        let first = self.sequence == 0;
        let mut current = HashMap::with_capacity(raw.processes.len());
        let mut spawned = Vec::new();
        for entry in &raw.processes {
            let key = entry.key();
            if !first && !self.previous.contains_key(&key) {
                spawned.push(key);
            }
            current.insert(
                key,
                ExitedProcess {
                    record: entry.record.clone(),
                    last_sample: entry.sample.clone(),
                },
            );
        }
        let previous = std::mem::replace(&mut self.previous, current);
        let exited = previous
            .into_iter()
            .filter(|(key, _)| !self.previous.contains_key(key))
            .map(|(_, exited)| exited)
            .collect();
        self.sequence += 1;
        ProcSnapshot {
            sequence: self.sequence,
            acquired_at_ms: now_ms(),
            interval_secs,
            system: raw.system,
            processes: raw.processes,
            spawned,
            exited,
            permission_limited: raw.permission_limited,
        }
    }

    fn deliver(&mut self) -> Flow {
        let Some(snapshot) = self.pending.take() else {
            return Flow::Continue;
        };
        match self.tx.try_send(snapshot) {
            Ok(()) => Flow::Continue,
            Err(TrySendError::Full(snapshot)) => {
                self.pending = Some(snapshot);
                Flow::Continue
            }
            Err(TrySendError::Closed(_)) => Flow::Stop,
        }
    }
}

/// Merge an undelivered snapshot into its successor so lifecycle events are
/// not lost: spawned keys that are still alive and every exit are kept.
fn coalesce(stale: ProcSnapshot, mut fresh: ProcSnapshot) -> ProcSnapshot {
    let live: HashSet<ProcessKey> = fresh.processes.iter().map(|e| e.key()).collect();
    let mut spawned: Vec<ProcessKey> = stale
        .spawned
        .into_iter()
        .filter(|key| live.contains(key))
        .collect();
    spawned.append(&mut fresh.spawned);
    fresh.spawned = spawned;
    let mut exited = stale.exited;
    exited.append(&mut fresh.exited);
    fresh.exited = exited;
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::model::{ProcessEntry, ProcessRecord, ProcessSample, SystemSnapshot};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::Receiver;

    /// Replays scripted process lists (the last one repeats forever).
    struct FakeSampler {
        script: Arc<Mutex<Vec<Vec<i32>>>>,
        calls: Arc<AtomicUsize>,
        priming: Option<Duration>,
    }

    impl FakeSampler {
        fn new(script: Vec<Vec<i32>>) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let sampler = Self {
                script: Arc::new(Mutex::new(script)),
                calls: calls.clone(),
                priming: None,
            };
            (sampler, calls)
        }
    }

    fn entry(pid: i32) -> ProcessEntry {
        ProcessEntry {
            record: Arc::new(ProcessRecord {
                key: ProcessKey::new(pid, 100),
                name: format!("p{pid}"),
                ..Default::default()
            }),
            sample: ProcessSample::default(),
        }
    }

    impl Sampler for FakeSampler {
        fn sample(&mut self) -> anyhow::Result<RawSample> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let script = self.script.lock().unwrap();
            let pids = script
                .get(call)
                .or_else(|| script.last())
                .cloned()
                .unwrap_or_default();
            Ok(RawSample {
                system: SystemSnapshot::default(),
                processes: pids.into_iter().map(entry).collect(),
                permission_limited: call,
            })
        }

        fn priming_delay(&self) -> Option<Duration> {
            self.priming
        }
    }

    struct Failing;
    impl Sampler for Failing {
        fn sample(&mut self) -> anyhow::Result<RawSample> {
            anyhow::bail!("nope")
        }
    }

    fn recv_within(rx: &mut Receiver<ProcSnapshot>, limit: Duration) -> Option<ProcSnapshot> {
        let deadline = Instant::now() + limit;
        loop {
            if let Ok(snapshot) = rx.try_recv() {
                return Some(snapshot);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pids(keys: &[ProcessKey]) -> Vec<i32> {
        let mut pids: Vec<i32> = keys.iter().map(|k| k.pid).collect();
        pids.sort_unstable();
        pids
    }

    const SLOW: Duration = Duration::from_secs(30);

    #[test]
    fn first_snapshot_is_immediate_and_has_no_lifecycle_events() {
        let (sampler, _) = FakeSampler::new(vec![vec![1, 2, 3]]);
        let (handle, mut rx) = spawn_with(Box::new(sampler), SLOW);
        let first = recv_within(&mut rx, Duration::from_secs(2)).expect("first snapshot");
        assert_eq!(first.sequence, 1);
        assert_eq!(first.processes.len(), 3);
        assert!(first.spawned.is_empty());
        assert!(first.exited.is_empty());
        assert_eq!(first.interval_secs, 0.0);
        handle.shutdown();
    }

    #[test]
    fn detects_spawned_and_exited() {
        let (sampler, _) = FakeSampler::new(vec![vec![1, 2], vec![2, 3, 4], vec![4]]);
        let (handle, mut rx) = spawn_with(Box::new(sampler), SLOW);
        let limit = Duration::from_secs(2);
        recv_within(&mut rx, limit).expect("first");
        handle.commands.send(CollectorCommand::RefreshNow).unwrap();
        let second = recv_within(&mut rx, limit).expect("second");
        assert_eq!(pids(&second.spawned), vec![3, 4]);
        let exited: Vec<ProcessKey> = second.exited.iter().map(|e| e.record.key).collect();
        assert_eq!(pids(&exited), vec![1]);
        assert!(second.interval_secs > 0.0);
        handle.commands.send(CollectorCommand::RefreshNow).unwrap();
        let third = recv_within(&mut rx, limit).expect("third");
        assert!(third.spawned.is_empty());
        let exited: Vec<ProcessKey> = third.exited.iter().map(|e| e.record.key).collect();
        assert_eq!(pids(&exited), vec![2, 3]);
        assert_eq!(
            third.exited[0].record.name,
            format!("p{}", third.exited[0].record.key.pid)
        );
        handle.shutdown();
    }

    #[test]
    fn pid_reuse_with_new_start_time_is_spawn_and_exit() {
        let mut worker_previous = HashMap::new();
        let old = entry(7);
        worker_previous.insert(
            old.key(),
            ExitedProcess {
                record: old.record.clone(),
                last_sample: old.sample.clone(),
            },
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let (_cmd_tx, cmd_rx) = std_mpsc::channel();
        let mut worker = Worker {
            sampler: Box::new(Failing),
            commands: cmd_rx,
            tx,
            interval: SLOW,
            paused: false,
            force: false,
            sequence: 1,
            previous: worker_previous,
            last_sample: None,
            next_due: Instant::now(),
            pending: None,
        };
        let mut reused = entry(7);
        Arc::make_mut(&mut reused.record).key = ProcessKey::new(7, 200);
        let snapshot = worker.build(
            RawSample {
                system: SystemSnapshot::default(),
                processes: vec![reused],
                permission_limited: 0,
            },
            1.0,
        );
        assert_eq!(snapshot.spawned, vec![ProcessKey::new(7, 200)]);
        assert_eq!(snapshot.exited[0].record.key, ProcessKey::new(7, 100));
    }

    #[test]
    fn pause_stops_ticks_and_refresh_now_still_samples() {
        let (sampler, calls) = FakeSampler::new(vec![vec![1]]);
        let (handle, mut rx) = spawn_with(Box::new(sampler), MIN_INTERVAL);
        recv_within(&mut rx, Duration::from_secs(2)).expect("first");
        handle.commands.send(CollectorCommand::Pause(true)).unwrap();
        // Let any in-flight tick land, then verify silence.
        std::thread::sleep(MIN_INTERVAL);
        while rx.try_recv().is_ok() {}
        let before = calls.load(Ordering::SeqCst);
        assert!(recv_within(&mut rx, MIN_INTERVAL * 3).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), before);

        let asked = Instant::now();
        handle.commands.send(CollectorCommand::RefreshNow).unwrap();
        recv_within(&mut rx, Duration::from_secs(1)).expect("refresh while paused");
        assert!(asked.elapsed() < MIN_INTERVAL);
        assert!(recv_within(&mut rx, MIN_INTERVAL * 2).is_none());

        handle
            .commands
            .send(CollectorCommand::Pause(false))
            .unwrap();
        let resumed = Instant::now();
        recv_within(&mut rx, Duration::from_secs(1)).expect("resume samples at once");
        assert!(resumed.elapsed() < MIN_INTERVAL);
        handle.shutdown();
    }

    #[test]
    fn refresh_now_is_prompt() {
        let (sampler, _) = FakeSampler::new(vec![vec![1]]);
        let (handle, mut rx) = spawn_with(Box::new(sampler), SLOW);
        recv_within(&mut rx, Duration::from_secs(2)).expect("first");
        let asked = Instant::now();
        handle.commands.send(CollectorCommand::RefreshNow).unwrap();
        let snapshot = recv_within(&mut rx, Duration::from_secs(1)).expect("refresh");
        assert_eq!(snapshot.sequence, 2);
        assert!(asked.elapsed() < Duration::from_millis(500));
        handle.shutdown();
    }

    #[test]
    fn interval_change_takes_effect_immediately() {
        let (sampler, _) = FakeSampler::new(vec![vec![1]]);
        let (handle, mut rx) = spawn_with(Box::new(sampler), SLOW);
        recv_within(&mut rx, Duration::from_secs(2)).expect("first");
        handle
            .commands
            .send(CollectorCommand::SetInterval(Duration::from_millis(1)))
            .unwrap();
        // Clamped to MIN_INTERVAL and anchored on the previous sample.
        let changed = Instant::now();
        recv_within(&mut rx, Duration::from_secs(2)).expect("tick at new interval");
        assert!(changed.elapsed() < MIN_INTERVAL * 2);
        handle.shutdown();
    }

    #[test]
    fn shutdown_does_not_block_on_undrained_receiver() {
        let (sampler, calls) = FakeSampler::new(vec![vec![1], vec![2], vec![3]]);
        let (handle, rx) = spawn_with(Box::new(sampler), MIN_INTERVAL);
        std::thread::sleep(MIN_INTERVAL * 5);
        assert!(
            calls.load(Ordering::SeqCst) >= 4,
            "keeps sampling while UI is busy"
        );
        let started = Instant::now();
        handle.shutdown();
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(rx);
    }

    #[test]
    fn exits_when_receiver_dropped() {
        let (sampler, _) = FakeSampler::new(vec![vec![1]]);
        let (handle, rx) = spawn_with(Box::new(sampler), MIN_INTERVAL);
        drop(rx);
        let started = Instant::now();
        while !handle.thread.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(2), "thread exits");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn exits_when_command_sender_dropped() {
        let (sampler, _) = FakeSampler::new(vec![vec![1]]);
        let (handle, _rx) = spawn_with(Box::new(sampler), SLOW);
        let CollectorHandle { commands, thread } = handle;
        drop(commands);
        let started = Instant::now();
        while !thread.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(2), "thread exits");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn slow_consumer_keeps_lifecycle_events() {
        let script: Vec<Vec<i32>> = (1..=8).map(|i| vec![i, i + 1]).collect();
        let (sampler, calls) = FakeSampler::new(script);
        let (handle, mut rx) = spawn_with(Box::new(sampler), MIN_INTERVAL);
        while calls.load(Ordering::SeqCst) < 8 {
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.commands.send(CollectorCommand::Pause(true)).unwrap();
        let mut snapshots = Vec::new();
        while let Some(s) = recv_within(&mut rx, MIN_INTERVAL) {
            snapshots.push(s);
        }
        handle.shutdown();
        assert!(snapshots.len() < 8, "stale snapshots were coalesced");
        assert!(snapshots.windows(2).all(|w| w[0].sequence < w[1].sequence));
        let exited: Vec<ProcessKey> = snapshots
            .iter()
            .flat_map(|s| s.exited.iter().map(|e| e.record.key))
            .collect();
        let delivered_exits = pids(&exited);
        let last = snapshots.last().expect("delivered");
        let alive: Vec<i32> = pids(&last.processes.iter().map(|e| e.key()).collect::<Vec<_>>());
        let first_pid = snapshots[0]
            .processes
            .iter()
            .map(|e| e.key().pid)
            .min()
            .unwrap();
        let expected: Vec<i32> = (first_pid..alive[0]).collect();
        assert_eq!(
            delivered_exits, expected,
            "every exit delivered exactly once"
        );
        for snapshot in &snapshots {
            for key in &snapshot.spawned {
                assert!(snapshot.processes.iter().any(|e| e.key() == *key));
            }
        }
    }

    #[test]
    fn priming_discards_the_warm_up_sample() {
        let (mut sampler, calls) = FakeSampler::new(vec![vec![1], vec![1, 2]]);
        sampler.priming = Some(Duration::from_millis(60));
        let (handle, mut rx) = spawn_with(Box::new(sampler), SLOW);
        let first = recv_within(&mut rx, Duration::from_secs(2)).expect("first");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(first.sequence, 1);
        assert_eq!(first.processes.len(), 2);
        assert!(first.spawned.is_empty(), "warm-up does not count as a tick");
        assert!(first.interval_secs >= 0.05);
        handle.shutdown();
    }

    #[test]
    fn shutdown_during_priming_is_prompt() {
        let (mut sampler, _) = FakeSampler::new(vec![vec![1]]);
        sampler.priming = Some(Duration::from_secs(30));
        let (handle, _rx) = spawn_with(Box::new(sampler), SLOW);
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        handle.shutdown();
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn sampler_errors_are_skipped() {
        let (handle, mut rx) = spawn_with(Box::new(Failing), MIN_INTERVAL);
        assert!(recv_within(&mut rx, MIN_INTERVAL * 2).is_none());
        let started = Instant::now();
        handle.shutdown();
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn intervals_are_clamped() {
        assert_eq!(clamp_interval(Duration::ZERO), DEFAULT_INTERVAL);
        assert_eq!(clamp_interval(Duration::from_millis(1)), MIN_INTERVAL);
        assert_eq!(
            clamp_interval(Duration::from_secs(2)),
            Duration::from_secs(2)
        );
        assert_eq!(clamp_interval(Duration::MAX), MAX_INTERVAL);
    }

    #[test]
    fn coalesce_keeps_live_spawns_and_all_exits() {
        let exited = |pid| ExitedProcess {
            record: entry(pid).record,
            last_sample: ProcessSample::default(),
        };
        let stale = ProcSnapshot {
            sequence: 4,
            spawned: vec![ProcessKey::new(5, 100), ProcessKey::new(6, 100)],
            exited: vec![exited(1)],
            ..Default::default()
        };
        let fresh = ProcSnapshot {
            sequence: 5,
            processes: vec![entry(5), entry(7)],
            spawned: vec![ProcessKey::new(7, 100)],
            exited: vec![exited(6)],
            ..Default::default()
        };
        let merged = coalesce(stale, fresh);
        assert_eq!(merged.sequence, 5);
        assert_eq!(pids(&merged.spawned), vec![5, 7]);
        let exits: Vec<ProcessKey> = merged.exited.iter().map(|e| e.record.key).collect();
        assert_eq!(pids(&exits), vec![1, 6]);
    }
}
