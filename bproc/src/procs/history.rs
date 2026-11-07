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
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }
    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
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
            dr += row.entry.sample.disk_read_rate;
            dw += row.entry.sample.disk_write_rate;
            if let Some(n) = &row.net {
                nd += n.download_rate;
                nu += n.upload_rate;
            }
        }
        self.disk_read.push(dr);
        self.disk_write.push(dw);
        self.net_down.push(nd);
        self.net_up.push(nu);
    }
}
