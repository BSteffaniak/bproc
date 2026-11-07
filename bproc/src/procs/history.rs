//! Bounded per-process and system metric history for sparklines and graphs.
use super::model::{ProcessSample, SystemSnapshot};
use super::store::Row;
use crate::process::ProcessInfo;
use std::collections::VecDeque;

/// Samples kept per series (at the default 1.5s interval ≈ 5 minutes).
pub const HISTORY_LEN: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct Ring<T> {
    values: VecDeque<T>,
    capacity: usize,
}

impl<T: Copy> Ring<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            values: VecDeque::with_capacity(capacity),
            capacity,
        }
    }
    pub fn push(&mut self, value: T) {
        if self.capacity == 0 {
            return;
        }
        while self.values.len() >= self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn last(&self) -> Option<T> {
        self.values.back().copied()
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = T> + ExactSizeIterator + '_ {
        self.values.iter().copied()
    }
    /// The most recent `n` values, oldest first.
    pub fn tail(&self, n: usize) -> Vec<T> {
        let skip = self.values.len().saturating_sub(n);
        self.values.iter().skip(skip).copied().collect()
    }
}

impl<T: Copy> Default for Ring<T> {
    fn default() -> Self {
        Self::new(HISTORY_LEN)
    }
}

impl Ring<u64> {
    pub fn max(&self) -> u64 {
        self.values.iter().copied().max().unwrap_or(0)
    }
}

impl Ring<f32> {
    pub fn max(&self) -> f32 {
        self.values.iter().copied().fold(0.0, f32::max)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetricHistory {
    pub cpu: Ring<f32>,
    pub mem_rss: Ring<u64>,
    pub disk_read: Ring<u64>,
    pub disk_write: Ring<u64>,
    pub net_down: Ring<u64>,
    pub net_up: Ring<u64>,
}

impl MetricHistory {
    pub fn push(&mut self, sample: &ProcessSample, net: Option<&ProcessInfo>) {
        self.cpu.push(sample.cpu_percent);
        self.mem_rss.push(sample.mem_rss);
        self.disk_read.push(sample.disk_read_rate);
        self.disk_write.push(sample.disk_write_rate);
        self.net_down
            .push(net.map(|n| n.download_rate).unwrap_or(0));
        self.net_up.push(net.map(|n| n.upload_rate).unwrap_or(0));
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemHistory {
    pub cpu_total: Ring<f32>,
    pub mem_used: Ring<u64>,
    pub swap_used: Ring<u64>,
    pub load1: Ring<f32>,
    pub disk_read: Ring<u64>,
    pub disk_write: Ring<u64>,
    pub net_down: Ring<u64>,
    pub net_up: Ring<u64>,
}

impl SystemHistory {
    pub fn push(&mut self, system: &SystemSnapshot, rows: &[Row]) {
        self.cpu_total.push(system.cpu_total);
        self.mem_used.push(system.mem_used);
        self.swap_used.push(system.swap_used);
        self.load1.push(system.load_average[0] as f32);
        let live = rows.iter().filter(|r| !r.is_ghost());
        let (mut dr, mut dw, mut nd, mut nu) = (0u64, 0u64, 0u64, 0u64);
        for row in live {
            dr = dr.saturating_add(row.entry.sample.disk_read_rate);
            dw = dw.saturating_add(row.entry.sample.disk_write_rate);
            if let Some(n) = &row.net {
                nd = nd.saturating_add(n.download_rate);
                nu = nu.saturating_add(n.upload_rate);
            }
        }
        self.disk_read.push(dr);
        self.disk_write.push(dw);
        self.net_down.push(nd);
        self.net_up.push(nu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_bounded_and_ordered() {
        let mut ring = Ring::new(3);
        for v in 1u64..=5 {
            ring.push(v);
        }
        assert_eq!(ring.iter().collect::<Vec<_>>(), vec![3, 4, 5]);
        assert_eq!(ring.tail(2), vec![4, 5]);
        assert_eq!(ring.tail(10), vec![3, 4, 5]);
        assert_eq!(ring.last(), Some(5));
        assert_eq!(ring.max(), 5);
        assert_eq!(ring.len(), 3);
    }

    #[test]
    fn empty_and_zero_capacity_rings_are_safe() {
        let ring: Ring<f32> = Ring::new(0);
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.max(), 0.0);
        assert_eq!(ring.tail(10), Vec::<f32>::new());
        let mut zero: Ring<u64> = Ring::new(0);
        zero.push(1);
        zero.push(2);
        assert_eq!(zero.len(), 0);
        assert_eq!(zero.last(), None);
    }

    #[test]
    fn non_finite_cpu_does_not_poison_max() {
        let mut ring: Ring<f32> = Ring::default();
        ring.push(f32::NAN);
        ring.push(2.0);
        assert_eq!(ring.max(), 2.0);
    }

    #[test]
    fn metric_history_advances_together() {
        let mut h = MetricHistory::default();
        let mut info = ProcessInfo::new(1, "a".into());
        info.download_rate = 7;
        h.push(&ProcessSample::default(), Some(&info));
        h.push(&ProcessSample::default(), None);
        assert_eq!(h.cpu.len(), 2);
        assert_eq!(h.mem_rss.len(), 2);
        assert_eq!(h.net_down.iter().collect::<Vec<_>>(), vec![7, 0]);
    }
}
