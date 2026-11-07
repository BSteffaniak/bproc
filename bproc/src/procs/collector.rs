//! Background process collector. Owns a `Sampler` on a dedicated thread and
//! publishes complete `ProcSnapshot`s; it never blocks the UI.
use super::model::{ExitedProcess, ProcSnapshot, ProcessKey, now_ms};
use super::platform::{Sampler, create_sampler};
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(1500);
pub const MIN_INTERVAL: Duration = Duration::from_millis(250);

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
    pub fn shutdown(self) {
        let _ = self.commands.send(CollectorCommand::Shutdown);
        let _ = self.thread.join();
    }
}

/// Spawn the collector. Snapshots are delivered on a bounded channel; when the
/// UI falls behind the collector waits rather than queueing stale data.
pub fn spawn(interval: Duration) -> (CollectorHandle, tokio::sync::mpsc::Receiver<ProcSnapshot>) {
    spawn_with(create_sampler(), interval)
}

pub fn spawn_with(
    mut sampler: Box<dyn Sampler>,
    interval: Duration,
) -> (CollectorHandle, tokio::sync::mpsc::Receiver<ProcSnapshot>) {
    let (cmd_tx, cmd_rx) = std_mpsc::channel();
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    let thread = std::thread::Builder::new()
        .name("bproc-collector".into())
        .spawn(move || {
            let mut interval = interval.max(MIN_INTERVAL);
            let mut paused = false;
            let mut sequence = 0u64;
            let mut previous: HashMap<ProcessKey, ExitedProcess> = HashMap::new();
            let mut last_tick: Option<Instant> = None;
            let mut first = true;
            loop {
                let wait = if first { Duration::ZERO } else { interval };
                first = false;
                let mut force = false;
                match cmd_rx.recv_timeout(wait) {
                    Ok(CollectorCommand::Shutdown)
                    | Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(CollectorCommand::SetInterval(d)) => {
                        interval = d.max(MIN_INTERVAL);
                        continue;
                    }
                    Ok(CollectorCommand::Pause(p)) => {
                        paused = p;
                        continue;
                    }
                    Ok(CollectorCommand::RefreshNow) => force = true,
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                }
                if paused && !force {
                    continue;
                }
                let raw = match sampler.sample() {
                    Ok(raw) => raw,
                    Err(error) => {
                        log::warn!("process sample failed: {error:#}");
                        continue;
                    }
                };
                let now = Instant::now();
                let interval_secs = last_tick
                    .map(|t| now.duration_since(t).as_secs_f32())
                    .unwrap_or(0.0);
                last_tick = Some(now);
                let mut current = HashMap::with_capacity(raw.processes.len());
                let mut spawned = Vec::new();
                for entry in &raw.processes {
                    let key = entry.key();
                    if sequence > 0 && !previous.contains_key(&key) {
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
                let exited = previous
                    .into_iter()
                    .filter(|(key, _)| !current.contains_key(key))
                    .map(|(_, exited)| exited)
                    .collect();
                previous = current;
                sequence += 1;
                let snapshot = ProcSnapshot {
                    sequence,
                    acquired_at_ms: now_ms(),
                    interval_secs,
                    system: raw.system,
                    processes: raw.processes,
                    spawned,
                    exited,
                    permission_limited: raw.permission_limited,
                };
                if tx.blocking_send(snapshot).is_err() {
                    return;
                }
            }
        })
        .expect("spawn collector thread");
    (
        CollectorHandle {
            commands: cmd_tx,
            thread,
        },
        rx,
    )
}
