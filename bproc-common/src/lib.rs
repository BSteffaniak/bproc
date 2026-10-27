#![no_std]

pub mod traffic;

/// Token bucket state for rate limiting
/// This struct is shared between userspace and eBPF programs via BPF maps
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TokenBucket {
    /// Maximum tokens (equals rate limit in bytes per second)
    pub capacity: u64,
    /// Current tokens available
    pub tokens: u64,
    /// Last update timestamp in nanoseconds
    pub last_update_ns: u64,
    /// Rate limit in bytes per second
    pub rate_bps: u64,
}

// SAFETY: TokenBucket is a plain old data type with all u64 fields
#[cfg(feature = "userspace")]
unsafe impl aya::Pod for TokenBucket {}

impl Default for TokenBucket {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenBucket {
    pub const fn new() -> Self {
        Self {
            capacity: 0,
            tokens: 0,
            last_update_ns: 0,
            rate_bps: 0,
        }
    }
}

/// Traffic type values for eBPF
pub const TRAFFIC_TYPE_ALL: u8 = 0;
pub const TRAFFIC_TYPE_INTERNET: u8 = 1;
pub const TRAFFIC_TYPE_LOCAL: u8 = 2;

/// Configuration for a cgroup throttle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CgroupThrottleConfig {
    /// cgroup inode number for matching
    pub cgroup_id: u64,
    /// PID of the process
    pub pid: u32,
    /// Traffic type to throttle (0=All, 1=Internet, 2=Local)
    pub traffic_type: u8,
    /// Padding for alignment (3 bytes to maintain 8-byte alignment)
    pub _padding: [u8; 3],
    /// Rate limit in bytes per second (sustained rate)
    pub rate_bps: u64,
    /// Burst size in bytes (maximum tokens, allows short bursts above rate)
    pub burst_size: u64,
}

// SAFETY: CgroupThrottleConfig is a plain old data type with all primitive fields
#[cfg(feature = "userspace")]
unsafe impl aya::Pod for CgroupThrottleConfig {}

impl Default for CgroupThrottleConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl CgroupThrottleConfig {
    pub const fn new() -> Self {
        Self {
            cgroup_id: 0,
            pid: 0,
            traffic_type: TRAFFIC_TYPE_ALL,
            _padding: [0, 0, 0], // Explicit array literal - [0; 3] causes LLVM errors in eBPF
            rate_bps: 0,
            burst_size: 0,
        }
    }
}

/// Statistics for a throttled cgroup
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ThrottleStats {
    /// Total packets seen
    pub packets_total: u64,
    /// Total bytes seen
    pub bytes_total: u64,
    /// Packets dropped due to rate limiting
    pub packets_dropped: u64,
    /// Bytes dropped due to rate limiting
    pub bytes_dropped: u64,
    /// Number of times eBPF program was called (diagnostic)
    pub program_calls: u64,
    /// Number of times cgroup not found in config map (diagnostic)
    pub config_misses: u64,
    /// The cgroup ID that the eBPF program actually sees (diagnostic)
    pub cgroup_id_seen: u64,
    /// Reserved for future use
    pub _reserved: u64,
}

// SAFETY: ThrottleStats is a plain old data type with all u64 fields
#[cfg(feature = "userspace")]
unsafe impl aya::Pod for ThrottleStats {}

impl Default for ThrottleStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ThrottleStats {
    pub const fn new() -> Self {
        Self {
            packets_total: 0,
            bytes_total: 0,
            packets_dropped: 0,
            bytes_dropped: 0,
            program_calls: 0,
            config_misses: 0,
            cgroup_id_seen: 0,
            _reserved: 0,
        }
    }
}
