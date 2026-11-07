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
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.capacity
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

/// Numeric sample types that can be plotted.
pub trait Metric: Copy {
    fn as_f64(self) -> f64;
}

impl Metric for f32 {
    fn as_f64(self) -> f64 {
        if self.is_finite() { self as f64 } else { 0.0 }
    }
}

impl Metric for u64 {
    fn as_f64(self) -> f64 {
        self as f64
    }
}

impl<T: Metric> Ring<T> {
    /// Largest value as `f64` (0 when empty), handy as a graph scale.
    pub fn peak(&self) -> f64 {
        self.values.iter().map(|v| v.as_f64()).fold(0.0, f64::max)
    }

    /// Mean of the stored values (0 when empty).
    pub fn mean(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        self.values.iter().map(|v| v.as_f64()).sum::<f64>() / self.values.len() as f64
    }

    /// The whole series fitted to `width` points, oldest first. See [`resample`].
    pub fn resampled(&self, width: usize) -> Vec<f64> {
        let values: Vec<f64> = self.values.iter().map(|v| v.as_f64()).collect();
        resample(&values, width)
    }
}

/// Fit `values` into at most `width` points for a sparkline. Longer series are
/// split into equal buckets keeping each bucket's maximum so short spikes stay
/// visible; shorter series are returned unchanged (callers right-align them).
pub fn resample(values: &[f64], width: usize) -> Vec<f64> {
    if width == 0 || values.is_empty() {
        return Vec::new();
    }
    if values.len() <= width {
        return values.to_vec();
    }
    let len = values.len();
    (0..width)
        .map(|i| {
            let start = i * len / width;
            let end = ((i + 1) * len / width).clamp(start + 1, len);
            values[start..end].iter().copied().fold(f64::MIN, f64::max)
        })
        .collect()
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

    /// Number of samples recorded (all series advance together).
    pub fn len(&self) -> usize {
        self.cpu.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cpu.is_empty()
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

    /// Number of samples recorded.
    pub fn len(&self) -> usize {
        self.cpu_total.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cpu_total.is_empty()
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
        assert_eq!(ring.peak(), 5.0);
        assert_eq!(ring.mean(), 4.0);
        assert_eq!(ring.capacity(), 3);
    }

    #[test]
    fn empty_and_zero_capacity_rings_are_safe() {
        let ring: Ring<f32> = Ring::new(0);
        assert!(ring.is_empty());
        assert_eq!(ring.max(), 0.0);
        assert_eq!(ring.mean(), 0.0);
        assert!(ring.resampled(10).is_empty());
        let mut zero: Ring<u64> = Ring::new(0);
        zero.push(1);
        zero.push(2);
        assert!(zero.is_empty());
        assert_eq!(zero.last(), None);
    }

    #[test]
    fn resample_keeps_peaks() {
        let mut values = vec![0.0; 100];
        values[37] = 9.0;
        let out = resample(&values, 10);
        assert_eq!(out.len(), 10);
        assert_eq!(out[3], 9.0);
        assert_eq!(out.iter().filter(|v| **v > 0.0).count(), 1);
        assert_eq!(resample(&[1.0, 2.0], 10), vec![1.0, 2.0]);
        assert!(resample(&[1.0], 0).is_empty());
        assert!(resample(&[], 4).is_empty());
        assert_eq!(resample(&[1.0, 5.0, 2.0], 1), vec![5.0]);
        let odd: Vec<f64> = (0..7).map(f64::from).collect();
        assert_eq!(resample(&odd, 3), vec![1.0, 3.0, 6.0]);
    }

    #[test]
    fn non_finite_cpu_plots_as_zero() {
        let mut ring: Ring<f32> = Ring::default();
        ring.push(f32::NAN);
        ring.push(2.0);
        assert_eq!(ring.resampled(5), vec![0.0, 2.0]);
        assert_eq!(ring.peak(), 2.0);
    }

    #[test]
    fn metric_history_advances_together() {
        let mut h = MetricHistory::default();
        let mut info = ProcessInfo::new(1, "a".into());
        info.download_rate = 7;
        h.push(&ProcessSample::default(), Some(&info));
        h.push(&ProcessSample::default(), None);
        assert_eq!(h.len(), 2);
        assert!(!h.is_empty());
        assert_eq!(h.net_down.iter().collect::<Vec<_>>(), vec![7, 0]);
    }
}
