use crate::db::{now_ms, Db};
use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
}

pub fn find_by_username(db: &Db, username: &str) -> Result<Option<User>, AppError> {
    db.with(|c| {
        let mut stmt =
            c.prepare("SELECT id, username, password_hash FROM user WHERE username = ?1")?;
        let mut rows = stmt.query([username])?;
        match rows.next()? {
            Some(row) => Ok(Some(User {
                id: row.get(0)?,
                username: row.get(1)?,
                password_hash: row.get(2)?,
            })),
            None => Ok(None),
        }
    })
}

/// Create the admin, or reset its password if it already exists — the same
/// behaviour as `_ensure_admin()` in the Flask app: ADMIN_PASSWORD is
/// authoritative on every start.
pub fn upsert_admin(db: &Db, username: &str, password_hash: &str) -> Result<(), AppError> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO user(username, password_hash, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash",
            (username, password_hash, now),
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_creates_then_updates() {
        let db = Db::open_in_memory().unwrap();
        upsert_admin(&db, "admin", "hash-1").unwrap();
        let user = find_by_username(&db, "admin").unwrap().unwrap();
        assert_eq!(user.password_hash, "hash-1");

        upsert_admin(&db, "admin", "hash-2").unwrap();
        let user = find_by_username(&db, "admin").unwrap().unwrap();
        assert_eq!(user.password_hash, "hash-2");

        let count: i64 = db
            .with(|c| c.query_row("SELECT count(*) FROM user", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(count, 1, "upsert must not duplicate the admin row");
    }

    #[test]
    fn unknown_user_is_none() {
        let db = Db::open_in_memory().unwrap();
        assert!(find_by_username(&db, "nobody").unwrap().is_none());
    }
}
