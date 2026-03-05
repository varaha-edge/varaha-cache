use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Epoch anchor for monotonic time. All VtimMono values are relative to this.
static MONO_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Monotonic time (seconds since unspecified epoch).
/// Used for measuring durations without wall-clock drift.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, Default)]
pub struct VtimMono(pub f64);

impl VtimMono {
    pub fn now() -> Self {
        Self(Instant::now().duration_since(*MONO_EPOCH).as_secs_f64())
    }

    pub fn elapsed_since(&self, earlier: VtimMono) -> VtimDur {
        VtimDur(self.0 - earlier.0)
    }
}

/// Real/wall-clock time (seconds since Unix epoch).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, Default)]
pub struct VtimReal(pub f64);

impl VtimReal {
    pub fn now() -> Self {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        Self(d.as_secs_f64())
    }

    pub fn from_secs(secs: f64) -> Self {
        Self(secs)
    }

    pub fn as_secs(&self) -> f64 {
        self.0
    }

    pub fn elapsed_since(&self, earlier: VtimReal) -> VtimDur {
        VtimDur(self.0 - earlier.0)
    }
}

/// Duration (seconds as f64).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, Default)]
pub struct VtimDur(pub f64);

impl VtimDur {
    pub const ZERO: Self = Self(0.0);

    pub fn from_secs(secs: f64) -> Self {
        Self(secs)
    }

    pub fn from_millis(ms: f64) -> Self {
        Self(ms / 1000.0)
    }

    pub fn as_secs(&self) -> f64 {
        self.0
    }

    pub fn as_millis(&self) -> f64 {
        self.0 * 1000.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0.0
    }

    pub fn is_positive(&self) -> bool {
        self.0 > 0.0
    }
}

impl From<Duration> for VtimDur {
    fn from(d: Duration) -> Self {
        Self(d.as_secs_f64())
    }
}

impl From<VtimDur> for Duration {
    fn from(d: VtimDur) -> Self {
        if d.0 <= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(d.0)
        }
    }
}

impl std::ops::Add<VtimDur> for VtimReal {
    type Output = VtimReal;
    fn add(self, rhs: VtimDur) -> VtimReal {
        VtimReal(self.0 + rhs.0)
    }
}

impl std::ops::Sub<VtimDur> for VtimReal {
    type Output = VtimReal;
    fn sub(self, rhs: VtimDur) -> VtimReal {
        VtimReal(self.0 - rhs.0)
    }
}

impl std::ops::Add for VtimDur {
    type Output = VtimDur;
    fn add(self, rhs: VtimDur) -> VtimDur {
        VtimDur(self.0 + rhs.0)
    }
}

impl std::ops::Sub for VtimDur {
    type Output = VtimDur;
    fn sub(self, rhs: VtimDur) -> VtimDur {
        VtimDur(self.0 - rhs.0)
    }
}

impl std::fmt::Display for VtimDur {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 >= 86400.0 {
            write!(f, "{:.1}d", self.0 / 86400.0)
        } else if self.0 >= 3600.0 {
            write!(f, "{:.1}h", self.0 / 3600.0)
        } else if self.0 >= 60.0 {
            write!(f, "{:.1}m", self.0 / 60.0)
        } else if self.0 >= 1.0 {
            write!(f, "{:.3}s", self.0)
        } else {
            write!(f, "{:.3}ms", self.0 * 1000.0)
        }
    }
}
