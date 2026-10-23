//! Persistence for the SMART monitor: disk state, samples, alerts.

use serde::Serialize;

use crate::db::Db;
use crate::error::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct DiskState {
    pub device: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub health: Option<String>,
    pub reallocated: i64,
    pub pending: i64,
    pub offline_uncorrectable: i64,
    pub ata_error_count: i64,
    pub temperature: Option<i64>,
    pub power_on_hours: Option<i64>,
    pub smartctl_rc: Option<i64>,
    pub resolved_counters: Option<String>,
    pub raw_summary: Option<String>,
    pub first_seen_at: i64,
    pub last_checked_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: i64,
    pub device: String,
    pub severity: String,
    pub message: String,
    pub active: bool,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub checked_at: i64,
    pub reallocated: i64,
    pub pending: i64,
    pub offline_uncorrectable: i64,
    pub ata_error_count: i64,
    pub temperature: Option<i64>,
    pub health: Option<String>,
    pub smartctl_rc: Option<i64>,
    pub source: String,
}

const STATE_COLUMNS: &str = "device, model, serial, health, reallocated, pending, \
     offline_uncorrectable, ata_error_count, temperature, power_on_hours, smartctl_rc, \
     resolved_counters, raw_summary, first_seen_at, last_checked_at";

fn state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiskState> {
    Ok(DiskState {
        device: row.get(0)?,
        model: row.get(1)?,
        serial: row.get(2)?,
        health: row.get(3)?,
        reallocated: row.get(4)?,
        pending: row.get(5)?,
        offline_uncorrectable: row.get(6)?,
        ata_error_count: row.get(7)?,
        temperature: row.get(8)?,
        power_on_hours: row.get(9)?,
        smartctl_rc: row.get(10)?,
        resolved_counters: row.get(11)?,
        raw_summary: row.get(12)?,
        first_seen_at: row.get(13)?,
        last_checked_at: row.get(14)?,
    })
}

pub fn find_state(db: &Db, device: &str) -> Result<Option<DiskState>, AppError> {
    db.with(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {STATE_COLUMNS} FROM smart_disk WHERE device = ?1"
        ))?;
        let mut rows = stmt.query([device])?;
        match rows.next()? {
            Some(row) => Ok(Some(state_from_row(row)?)),
            None => Ok(None),
        }
    })
}

pub fn list_states(db: &Db) -> Result<Vec<DiskState>, AppError> {
    db.with(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {STATE_COLUMNS} FROM smart_disk ORDER BY device"
        ))?;
        let rows = stmt.query_map([], state_from_row)?;
        rows.collect()
    })
}

pub fn active_alerts(db: &Db) -> Result<Vec<Alert>, AppError> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, device, severity, message, active, created_at, resolved_at
             FROM smart_alert WHERE active = 1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Alert {
                id: row.get(0)?,
                device: row.get(1)?,
                severity: row.get(2)?,
                message: row.get(3)?,
                active: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
                resolved_at: row.get(6)?,
            })
        })?;
        rows.collect()
    })
}

pub fn get_alert(db: &Db, id: i64) -> Result<Option<Alert>, AppError> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, device, severity, message, active, created_at, resolved_at
             FROM smart_alert WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Alert {
                id: row.get(0)?,
                device: row.get(1)?,
                severity: row.get(2)?,
                message: row.get(3)?,
                active: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
                resolved_at: row.get(6)?,
            })),
            None => Ok(None),
        }
    })
}

pub fn insert_alert(
    db: &Db,
    device: &str,
    severity: &str,
    message: &str,
    counters: &str,
    now: i64,
) -> Result<(), AppError> {
    db.with(|conn| {
        conn.execute(
            "INSERT INTO smart_alert(device, severity, message, active, created_at, alert_counters)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            (device, severity, message, now, counters),
        )
        .map(|_| ())
    })
}

/// Every run resolves the previous alerts for a device; a surviving issue is
/// re-raised as a fresh alert with the current counters.
pub fn resolve_alerts_for_device(db: &Db, device: &str, now: i64) -> Result<(), AppError> {
    db.with(|conn| {
        conn.execute(
            "UPDATE smart_alert SET active = 0, resolved_at = ?2
             WHERE device = ?1 AND active = 1",
            (device, now),
        )
        .map(|_| ())
    })
}

pub fn resolve_alert(db: &Db, id: i64, now: i64) -> Result<(), AppError> {
    db.with(|conn| {
        conn.execute(
            "UPDATE smart_alert SET active = 0, resolved_at = ?2 WHERE id = ?1",
            (id, now),
        )
        .map(|_| ())
    })
}

pub struct SampleInsert<'a> {
    pub device: &'a str,
    pub reallocated: i64,
    pub pending: i64,
    pub offline_uncorrectable: i64,
    pub ata_error_count: i64,
    pub temperature: Option<i64>,
    pub health: &'a str,
    pub smartctl_rc: i64,
    pub source: &'a str,
    pub now: i64,
}

pub fn insert_sample(db: &Db, sample: &SampleInsert<'_>) -> Result<(), AppError> {
    db.with(|conn| {
        conn.execute(
            "INSERT INTO smart_sample(device, reallocated, pending, offline_uncorrectable,
                 ata_error_count, temperature, health, smartctl_rc, checked_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                sample.device,
                sample.reallocated,
                sample.pending,
                sample.offline_uncorrectable,
                sample.ata_error_count,
                sample.temperature,
                sample.health,
                sample.smartctl_rc,
                sample.now,
                sample.source,
            ),
        )
        .map(|_| ())
    })
}

pub fn history(db: &Db, device: &str, limit: i64) -> Result<Vec<Sample>, AppError> {
    db.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT checked_at, reallocated, pending, offline_uncorrectable, ata_error_count,
                    temperature, health, smartctl_rc, source
             FROM smart_sample WHERE device = ?1
             ORDER BY checked_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map((device, limit), |row| {
            Ok(Sample {
                checked_at: row.get(0)?,
                reallocated: row.get(1)?,
                pending: row.get(2)?,
                offline_uncorrectable: row.get(3)?,
                ata_error_count: row.get(4)?,
                temperature: row.get(5)?,
                health: row.get(6)?,
                smartctl_rc: row.get(7)?,
                source: row.get(8)?,
            })
        })?;
        rows.collect()
    })
}

pub struct StateUpsert<'a> {
    pub device: &'a str,
    pub model: Option<&'a str>,
    pub serial: Option<&'a str>,
    pub health: &'a str,
    pub reallocated: i64,
    pub pending: i64,
    pub offline_uncorrectable: i64,
    pub ata_error_count: i64,
    pub temperature: Option<i64>,
    pub power_on_hours: Option<i64>,
    pub smartctl_rc: i64,
    pub now: i64,
}

pub fn upsert_state(db: &Db, state: &StateUpsert<'_>) -> Result<(), AppError> {
    db.with(|conn| {
        conn.execute(
            "INSERT INTO smart_disk(
                 device, model, serial, health, reallocated, pending, offline_uncorrectable,
                 ata_error_count, temperature, power_on_hours, smartctl_rc, raw_summary,
                 first_seen_at, last_checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?2, ?12, ?12)
             ON CONFLICT(device) DO UPDATE SET
                 model = excluded.model,
                 serial = excluded.serial,
                 health = excluded.health,
                 reallocated = excluded.reallocated,
                 pending = excluded.pending,
                 offline_uncorrectable = excluded.offline_uncorrectable,
                 ata_error_count = excluded.ata_error_count,
                 temperature = excluded.temperature,
                 power_on_hours = excluded.power_on_hours,
                 smartctl_rc = excluded.smartctl_rc,
                 last_checked_at = excluded.last_checked_at",
            rusqlite::params![
                state.device,
                state.model,
                state.serial,
                state.health,
                state.reallocated,
                state.pending,
                state.offline_uncorrectable,
                state.ata_error_count,
                state.temperature,
                state.power_on_hours,
                state.smartctl_rc,
                state.now,
            ],
        )
        .map(|_| ())
    })
}

/// Store the current counters as the baseline for future comparisons.
pub fn save_baseline(
    db: &Db,
    device: &str,
    reallocated: i64,
    pending: i64,
    offline: i64,
    ata_errors: i64,
) -> Result<(), AppError> {
    let baseline = serde_json::json!({
        "reallocated": reallocated,
        "pending": pending,
        "offline": offline,
        "ata_errors": ata_errors,
    })
    .to_string();
    db.with(|conn| {
        conn.execute(
            "UPDATE smart_disk SET resolved_counters = ?2 WHERE device = ?1",
            (device, baseline),
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(device: &'a str, now: i64) -> SampleInsert<'a> {
        SampleInsert {
            device,
            reallocated: 0,
            pending: 1,
            offline_uncorrectable: 0,
            ata_error_count: 2,
            temperature: Some(40),
            health: "PASSED",
            smartctl_rc: 0,
            source: "auto",
            now,
        }
    }

    #[test]
    fn state_upsert_preserves_first_seen_and_baseline() {
        let db = Db::open_in_memory().unwrap();
        let first = StateUpsert {
            device: "/dev/sda",
            model: Some("Model"),
            serial: Some("Serial"),
            health: "PASSED",
            reallocated: 0,
            pending: 0,
            offline_uncorrectable: 0,
            ata_error_count: 0,
            temperature: Some(40),
            power_on_hours: Some(10),
            smartctl_rc: 0,
            now: 1000,
        };
        upsert_state(&db, &first).unwrap();
        save_baseline(&db, "/dev/sda", 1, 2, 3, 4).unwrap();

        let second = StateUpsert {
            reallocated: 5,
            now: 2000,
            ..first
        };
        upsert_state(&db, &second).unwrap();

        let state = find_state(&db, "/dev/sda").unwrap().unwrap();
        assert_eq!(state.reallocated, 5);
        assert_eq!(state.first_seen_at, 1000);
        assert_eq!(state.last_checked_at, 2000);
        assert!(state.resolved_counters.unwrap().contains("\"ata_errors\":4"));
    }

    #[test]
    fn history_is_newest_first_and_limited() {
        let db = Db::open_in_memory().unwrap();
        for now in [10, 30, 20, 40] {
            insert_sample(&db, &sample("/dev/sda", now)).unwrap();
        }
        let rows = history(&db, "/dev/sda", 3).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].checked_at, 40);
        assert_eq!(rows[2].checked_at, 20);
    }

    #[test]
    fn alerts_resolve_and_list() {
        let db = Db::open_in_memory().unwrap();
        insert_alert(&db, "/dev/sda", "warning", "one", "{}", 1).unwrap();
        assert_eq!(active_alerts(&db).unwrap().len(), 1);
        resolve_alert(&db, 1, 2).unwrap();
        assert!(active_alerts(&db).unwrap().is_empty());
        assert_eq!(get_alert(&db, 1).unwrap().unwrap().resolved_at, Some(2));
    }

    #[test]
    fn resolving_by_device_closes_all_active_alerts() {
        let db = Db::open_in_memory().unwrap();
        insert_alert(&db, "/dev/sda", "warning", "one", "{}", 1).unwrap();
        insert_alert(&db, "/dev/sda", "critical", "two", "{}", 2).unwrap();
        insert_alert(&db, "/dev/sdb", "warning", "other disk", "{}", 3).unwrap();
        resolve_alerts_for_device(&db, "/dev/sda", 4).unwrap();
        let open = active_alerts(&db).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].device, "/dev/sdb");
    }
}
