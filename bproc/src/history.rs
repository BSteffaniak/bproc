// Bandwidth history tracking for graphing

use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of history samples to keep (e.g., 60 samples = 1 minute at 1Hz)
const MAX_HISTORY_SAMPLES: usize = 60;

/// A single bandwidth measurement sample
pub type GraphPoints = Vec<(f64, f64)>;

#[derive(Debug, Clone)]
pub struct BandwidthSample {
    // Retained domain metadata for platform collectors and diagnostics.
    #[allow(dead_code)]
    pub timestamp: u64, // Unix timestamp in seconds
    pub download_rate: u64, // bytes per second
    pub upload_rate: u64,   // bytes per second
}

/// Bandwidth history for a single process
#[derive(Debug, Clone)]
pub struct ProcessHistory {
    // Retained domain metadata for platform collectors and diagnostics.
    #[allow(dead_code)]
    pub pid: i32,
    pub process_name: String,
    pub samples: VecDeque<BandwidthSample>,
}

impl ProcessHistory {
    pub fn new(pid: i32, process_name: String) -> Self {
        Self {
            pid,
            process_name,
            samples: VecDeque::with_capacity(MAX_HISTORY_SAMPLES),
        }
    }

    /// Add a new sample, removing old ones if we exceed the limit
    #[cfg(test)]
    pub fn add_sample(&mut self, download_rate: u64, upload_rate: u64) {
        self.add_sample_at(unix_timestamp(), download_rate, upload_rate);
    }

    /// Append a sample at its acquisition time, independently of presentation cadence.
    pub fn add_sample_at(&mut self, timestamp: u64, download_rate: u64, upload_rate: u64) {
        let sample = BandwidthSample {
            timestamp,
            download_rate,
            upload_rate,
        };

        self.samples.push_back(sample);

        // Remove old samples if we exceed the limit
        while self.samples.len() > MAX_HISTORY_SAMPLES {
            self.samples.pop_front();
        }
    }

    /// Get the maximum download rate in history
    pub fn max_download_rate(&self) -> u64 {
        self.samples
            .iter()
            .map(|s| s.download_rate)
            .max()
            .unwrap_or(0)
    }

    /// Get the maximum upload rate in history
    pub fn max_upload_rate(&self) -> u64 {
        self.samples
            .iter()
            .map(|s| s.upload_rate)
            .max()
            .unwrap_or(0)
    }

    /// Get the average download rate
    pub fn avg_download_rate(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let sum: u64 = self.samples.iter().map(|s| s.download_rate).sum();
        sum / self.samples.len() as u64
    }

    /// Get the average upload rate
    pub fn avg_upload_rate(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let sum: u64 = self.samples.iter().map(|s| s.upload_rate).sum();
        sum / self.samples.len() as u64
    }

    /// Get samples as vectors for graphing
    pub fn get_graph_data(&self) -> (GraphPoints, GraphPoints) {
        let download_data: Vec<(f64, f64)> = self
            .samples
            .iter()
            .enumerate()
            .map(|(i, s)| (i as f64, s.download_rate as f64))
            .collect();

        let upload_data: Vec<(f64, f64)> = self
            .samples
            .iter()
            .enumerate()
            .map(|(i, s)| (i as f64, s.upload_rate as f64))
            .collect();

        (download_data, upload_data)
    }
}

/// Global history tracker for all processes
#[derive(Debug)]
pub struct HistoryTracker {
    pub histories: HashMap<i32, ProcessHistory>,
}

impl HistoryTracker {
    pub fn new() -> Self {
        Self {
            histories: HashMap::new(),
        }
    }

    /// Update history for a process
    #[cfg(test)]
    pub fn update(&mut self, pid: i32, process_name: String, download_rate: u64, upload_rate: u64) {
        self.update_at(
            unix_timestamp(),
            pid,
            process_name,
            download_rate,
            upload_rate,
        );
    }

    /// Record a reliably delivered measurement without replacing intermediate samples.
    pub fn update_at(
        &mut self,
        timestamp: u64,
        pid: i32,
        process_name: String,
        download_rate: u64,
        upload_rate: u64,
    ) {
        let history = self
            .histories
            .entry(pid)
            .or_insert_with(|| ProcessHistory::new(pid, process_name.clone()));

        // Update process name in case it changed
        history.process_name = process_name;
        history.add_sample_at(timestamp, download_rate, upload_rate);
    }

    /// Get history for a specific process
    pub fn get_history(&self, pid: i32) -> Option<&ProcessHistory> {
        self.histories.get(&pid)
    }
}

impl Default for HistoryTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_history() {
        let mut history = ProcessHistory::new(1234, "test".to_string());

        history.add_sample(1000, 500);
        history.add_sample(2000, 1000);
        history.add_sample(1500, 750);

        assert_eq!(history.samples.len(), 3);
        assert_eq!(history.max_download_rate(), 2000);
        assert_eq!(history.max_upload_rate(), 1000);
        assert_eq!(history.avg_download_rate(), 1500);
        assert_eq!(history.avg_upload_rate(), 750);
    }

    #[test]
    fn reliable_samples_preserve_peaks_and_acquisition_times() {
        let mut tracker = HistoryTracker::new();
        for (timestamp, download, upload) in [(10, 1, 3), (11, 900, 700), (12, 2, 4)] {
            tracker.update_at(timestamp, 7, "worker".into(), download, upload);
        }
        let history = tracker.get_history(7).unwrap();
        assert_eq!(
            history
                .samples
                .iter()
                .map(|s| s.timestamp)
                .collect::<Vec<_>>(),
            [10, 11, 12]
        );
        assert_eq!(history.max_download_rate(), 900);
        assert_eq!(history.max_upload_rate(), 700);
        assert_eq!(
            history.get_graph_data().0,
            [(0.0, 1.0), (1.0, 900.0), (2.0, 2.0)]
        );
    }

    #[test]
    fn timestamped_history_retains_latest_window_and_updated_name() {
        let mut tracker = HistoryTracker::new();
        for timestamp in 0..70 {
            tracker.update_at(timestamp, 7, format!("worker-{timestamp}"), timestamp, 0);
        }
        let history = tracker.get_history(7).unwrap();
        assert_eq!(history.samples.len(), 60);
        assert_eq!(history.samples.front().unwrap().timestamp, 10);
        assert_eq!(history.samples.back().unwrap().timestamp, 69);
        assert_eq!(history.process_name, "worker-69");
    }

    #[test]
    fn test_history_limit() {
        let mut history = ProcessHistory::new(1234, "test".to_string());

        // Add more than MAX_HISTORY_SAMPLES
        for i in 0..(MAX_HISTORY_SAMPLES + 10) {
            history.add_sample(i as u64, i as u64);
        }

        assert_eq!(history.samples.len(), MAX_HISTORY_SAMPLES);
    }
}
