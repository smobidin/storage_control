//! Cron parsing shared by the backup tasks and the scheduled-job registry.
//!
//! The Flask app used APScheduler and a `apscheduler_jobs` table; here the
//! schedule is just a five-field cron string in the task row and the registry
//! is rebuilt in memory from the database. One parser for both keeps the
//! "is this cron valid?" answer identical in the UI and at runtime.

use chrono::{DateTime, Local};

/// Parse a five- or six-field cron expression (seconds optional).
pub fn parse_cron(expression: &str) -> Result<croner::Cron, String> {
    let mut cron = croner::Cron::new(expression.trim());
    cron.parse().map_err(|e| e.to_string())
}

/// Next occurrence strictly after `after`. Invalid expressions yield `None`;
/// callers validate on write, so this only happens if the row was edited by
/// hand.
pub fn next_run(expression: &str, after: DateTime<Local>) -> Option<DateTime<Local>> {
    let cron = parse_cron(expression).ok()?;
    cron.find_next_occurrence(&after, false).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn five_field_cron_is_valid() {
        assert!(parse_cron("0 4 * * *").is_ok());
        assert!(parse_cron("*/15 2 * * 1-5").is_ok());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_cron("").is_err());
        assert!(parse_cron("every day").is_err());
        assert!(parse_cron("0 4 * *").is_err());
    }

    #[test]
    fn next_run_is_in_the_future() {
        let base = Local.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap();
        let next = next_run("0 4 * * *", base).unwrap();
        assert_eq!(next.format("%Y-%m-%d %H:%M").to_string(), "2026-10-24 04:00");
    }
}
