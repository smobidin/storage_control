//! Background jobs. Everything here runs in `spawn_blocking` or a dedicated
//! thread: collectors shell out to smartctl/systemctl, which must never block
//! the async runtime.

pub mod backup;
pub mod preserve;
pub mod scheduler;
pub mod smart_monitor;
pub mod update_immich;
pub mod update_nextcloud;
pub mod update_stalwart;
