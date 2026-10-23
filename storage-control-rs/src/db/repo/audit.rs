use crate::db::{now_ms, Db};


/// Append an audit row. Failures are logged, never propagated: losing an audit
/// line must not fail the action the user asked for.
pub fn record(db: &Db, actor: &str, action: &str, object: Option<&str>, payload: Option<&str>) {
    let now = now_ms();
    let result = db.with(|c| {
        c.execute(
            "INSERT INTO audit(actor, action, object, payload, ip, ts) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            (actor, action, object, payload, now),
        )
    });
    if let Err(e) = result {
        tracing::warn!("audit write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_row() {
        let db = Db::open_in_memory().unwrap();
        record(&db, "admin", "service.restart", Some("nginx.service"), Some("ok"));
        let (action, object): (String, Option<String>) = db
            .with(|c| {
                c.query_row("SELECT action, object FROM audit ORDER BY id DESC LIMIT 1", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
            })
            .unwrap();
        assert_eq!(action, "service.restart");
        assert_eq!(object.as_deref(), Some("nginx.service"));
    }
}
