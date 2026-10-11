//! Run tests inside the booted image.

mod accounts;
mod arch;
mod boot;
mod debian;
mod fedora;
mod ubuntu;
mod units;

use std::time::{Duration, SystemTime};

use rustix::time::{ClockId, clock_gettime};

/// The time the system booted.
fn boot_time() -> SystemTime {
    let since_boot = clock_gettime(ClockId::Boottime);
    let since_boot = Duration::new(
        u64::try_from(since_boot.tv_sec).expect("a positive uptime"),
        u32::try_from(since_boot.tv_nsec).expect("nanoseconds below a second"),
    );
    SystemTime::now() - since_boot
}
