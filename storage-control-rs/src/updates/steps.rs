//! Step tracking helper for update runners.
//!
//! Mirrors the Python `_push` / `step` pattern: every step is appended to a
//! JSON array that is persisted to the database immediately so the UI can
//! poll live progress.

use crate::db::Db;
use crate::error::AppError;
use serde_json::json;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Step {
    pub name: String,
    pub status: String, // "running" | "ok" | "failed"
    pub rc: Option<i32>,
}

impl Step {
    pub fn running(name: &str) -> Self {
        Self { name: name.into(), status: "running".into(), rc: None }
    }
    pub fn finished(name: &str, rc: i32) -> Self {
        Self { name: name.into(), status: if rc == 0 { "ok" } else { "failed" }.into(), rc: Some(rc) }
    }
}

pub struct StepTracker {
    db: Db,
    run_id: i64,
    steps: Vec<Step>,
}

impl StepTracker {
    pub fn new(db: Db, run_id: i64) -> Self {
        Self { db, run_id, steps: Vec::new() }
    }

    /// Run a step, persist it immediately, and return its CmdOutput.
    pub fn step<F>(&mut self, name: &str, f: F) -> Result<crate::exec::CmdOutput, AppError>
    where
        F: FnOnce() -> Result<crate::exec::CmdOutput, AppError>,
    {
        self.steps.push(Step::running(name));
        self.persist()?;

        let out = f()?;
        let rc = out.rc;
        self.steps.last_mut().unwrap().status = if rc == 0 { "ok" } else { "failed" }.into();
        self.steps.last_mut().unwrap().rc = Some(rc);
        self.persist()?;
        Ok(out)
    }

    /// Run a step that returns `(rc, output)` like the shell helpers.
    pub fn step_raw<F>(&mut self, name: &str, f: F) -> Result<i32, AppError>
    where
        F: FnOnce() -> Result<(i32, String), AppError>,
    {
        self.steps.push(Step::running(name));
        self.persist()?;

        let (rc, _out) = f()?;
        self.steps.last_mut().unwrap().status = if rc == 0 { "ok" } else { "failed" }.into();
        self.steps.last_mut().unwrap().rc = Some(rc);
        self.persist()?;
        Ok(rc)
    }

    fn persist(&self) -> Result<(), AppError> {
        self.db.with(|c| {
            c.execute(
                "UPDATE update_run SET steps = ?1 WHERE id = ?2",
                (json!(self.steps).to_string(), self.run_id),
            )?;
            Ok(())
        })
    }

    pub fn into_steps(self) -> Vec<Step> {
        self.steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn tracker_persists_each_step() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| -> rusqlite::Result<()> {
            c.execute(
                "INSERT INTO update_run(id, target, status, started_at) VALUES (1, 'immich', 'running', 0)",
                [],
            )?;
            Ok(())
        }).unwrap();
        let mut tracker = StepTracker::new(db.clone(), 1);
        let _ = tracker.step("step1", || Ok(crate::exec::CmdOutput { rc: 0, stdout: "".into(), stderr: "".into() })).unwrap();
        let _ = tracker.step("step2", || Ok(crate::exec::CmdOutput { rc: 1, stdout: "".into(), stderr: "err".into() })).unwrap();
        let steps = tracker.into_steps();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].status, "ok");
        assert_eq!(steps[1].status, "failed");
    }
}