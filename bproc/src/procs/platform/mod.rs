//! Platform samplers. `SysinfoSampler` is the portable base; platform modules
//! enrich samples with fields sysinfo does not expose (nice, priority, threads,
//! fds, context switches, kernel threads, tty, cgroup/container).
use super::model::{ProcessEntry, SystemSnapshot};
use anyhow::Result;

mod base;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

pub use base::SysinfoSampler;

/// Raw measurements for one tick. Rates are computed by the sampler from the
/// previous tick it observed; the collector only orchestrates timing.
pub struct RawSample {
    pub system: SystemSnapshot,
    pub processes: Vec<ProcessEntry>,
    pub permission_limited: usize,
}

pub trait Sampler: Send {
    fn sample(&mut self) -> Result<RawSample>;
}

/// Pick the best sampler for this host.
pub fn create_sampler() -> Box<dyn Sampler> {
    Box::new(SysinfoSampler::new())
}
