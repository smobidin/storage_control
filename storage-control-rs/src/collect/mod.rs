//! Read-only host collectors: system overview, RAID, SMART, systemd services.
//!
//! Parsers are ported from `storage-control/app/collectors/`; the regexes are
//! deliberately identical because they were tuned against real smartctl /
//! mdadm output.

pub mod backups;
pub mod raid;
pub mod services;
pub mod smart;
pub mod system;
