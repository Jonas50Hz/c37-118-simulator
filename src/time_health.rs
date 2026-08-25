use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

#[derive(Debug, Clone)]
pub enum TimeSynchronizationSource {
    AlwaysVerified,
    Unverified,
    File { path: PathBuf },
}

impl TimeSynchronizationSource {
    pub fn is_verified(&self) -> bool {
        match self {
            Self::AlwaysVerified => true,
            Self::Unverified => false,
            Self::File { path } => match std::fs::read_to_string(path) {
                Ok(contents) => Self::file_contents_are_verified(&contents),
                Err(_) => false,
            },
        }
    }

    fn file_contents_are_verified(contents: &str) -> bool {
        contents.trim_matches(|character: char| character.is_ascii_whitespace()) == "verified"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeHealthState {
    Unobserved,
    Verified,
    SynchronizationUnverified,
    MaterialClockRegression,
}

impl TimeHealthState {
    pub const fn is_degraded(self) -> bool {
        !matches!(self, Self::Verified)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unobserved => "unobserved",
            Self::Verified => "verified",
            Self::SynchronizationUnverified => "synchronization_unverified",
            Self::MaterialClockRegression => "material_clock_regression",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimeHealthMonitor {
    reporting_interval: Duration,
    last_observed_wall_clock: Option<SystemTime>,
}

impl TimeHealthMonitor {
    pub fn new(reporting_interval: Duration) -> Self {
        Self {
            reporting_interval,
            last_observed_wall_clock: None,
        }
    }

    pub fn observe_boundary(
        &mut self,
        observed_wall_clock: SystemTime,
        synchronization_verified: bool,
    ) -> TimeHealthState {
        let state = if observed_wall_clock.duration_since(UNIX_EPOCH).is_err()
            || !synchronization_verified
        {
            TimeHealthState::SynchronizationUnverified
        } else if self.has_material_clock_regression(observed_wall_clock) {
            TimeHealthState::MaterialClockRegression
        } else {
            TimeHealthState::Verified
        };

        self.last_observed_wall_clock = Some(observed_wall_clock);
        state
    }

    fn has_material_clock_regression(&self, observed_wall_clock: SystemTime) -> bool {
        let Some(last_observed_wall_clock) = self.last_observed_wall_clock else {
            return false;
        };

        let Err(backward_movement) = observed_wall_clock.duration_since(last_observed_wall_clock)
        else {
            return false;
        };

        backward_movement.duration() > self.reporting_interval
    }
}

#[cfg(test)]
mod tests {
    use super::{TimeHealthMonitor, TimeHealthState, TimeSynchronizationSource};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn wall_clock_at(seconds_after_epoch: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds_after_epoch)
    }

    #[test]
    fn initial_unverified_boundary_is_degraded() {
        let mut monitor = TimeHealthMonitor::new(Duration::from_secs(5));

        let state = monitor.observe_boundary(wall_clock_at(100), false);

        assert_eq!(state, TimeHealthState::SynchronizationUnverified);
        assert!(state.is_degraded());
    }

    #[test]
    fn verified_initial_boundary_is_healthy() {
        let mut monitor = TimeHealthMonitor::new(Duration::from_secs(5));

        let state = monitor.observe_boundary(wall_clock_at(100), true);

        assert_eq!(state, TimeHealthState::Verified);
        assert!(!state.is_degraded());
    }

    #[test]
    fn material_backward_movement_is_degraded_despite_verified_synchronization() {
        let mut monitor = TimeHealthMonitor::new(Duration::from_secs(5));
        monitor.observe_boundary(wall_clock_at(100), true);

        let state = monitor.observe_boundary(wall_clock_at(94), true);

        assert_eq!(state, TimeHealthState::MaterialClockRegression);
        assert!(state.is_degraded());
    }

    #[test]
    fn one_reporting_interval_or_less_of_backward_movement_is_accepted() {
        for backward_seconds in [4, 5] {
            let mut monitor = TimeHealthMonitor::new(Duration::from_secs(5));
            monitor.observe_boundary(wall_clock_at(100), true);

            let state = monitor.observe_boundary(wall_clock_at(100 - backward_seconds), true);

            assert_eq!(state, TimeHealthState::Verified);
            assert!(!state.is_degraded());
        }
    }

    #[test]
    fn verified_boundary_recovers_from_material_clock_regression() {
        let mut monitor = TimeHealthMonitor::new(Duration::from_secs(5));
        monitor.observe_boundary(wall_clock_at(100), true);
        assert_eq!(
            monitor.observe_boundary(wall_clock_at(94), true),
            TimeHealthState::MaterialClockRegression
        );

        let state = monitor.observe_boundary(wall_clock_at(95), true);

        assert_eq!(state, TimeHealthState::Verified);
        assert!(!state.is_degraded());
    }

    #[test]
    fn verified_boundary_recovers_from_unverified_synchronization() {
        let mut monitor = TimeHealthMonitor::new(Duration::from_secs(100));
        assert_eq!(
            monitor.observe_boundary(wall_clock_at(100), false),
            TimeHealthState::SynchronizationUnverified
        );

        let state = monitor.observe_boundary(wall_clock_at(101), true);

        assert_eq!(state, TimeHealthState::Verified);
        assert!(!state.is_degraded());
    }

    #[test]
    fn wall_clock_before_epoch_is_unverified_without_panicking() {
        let mut monitor = TimeHealthMonitor::new(Duration::from_secs(5));
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_secs(1))
            .expect("one second before the epoch is representable");

        let state = monitor.observe_boundary(before_epoch, true);

        assert_eq!(state, TimeHealthState::SynchronizationUnverified);
        assert!(state.is_degraded());
    }

    #[test]
    fn synchronization_sources_require_exact_verified_status_text() {
        assert!(TimeSynchronizationSource::AlwaysVerified.is_verified());
        assert!(!TimeSynchronizationSource::Unverified.is_verified());
        assert!(TimeSynchronizationSource::file_contents_are_verified(
            " \tverified\r\n"
        ));
        assert!(!TimeSynchronizationSource::file_contents_are_verified(
            "verified\nready"
        ));
        assert!(!TimeSynchronizationSource::file_contents_are_verified(
            "Verified"
        ));
        assert!(!TimeSynchronizationSource::file_contents_are_verified(
            "\u{00a0}verified"
        ));
    }
}