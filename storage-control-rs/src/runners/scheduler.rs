//! Scheduler runner — cron-based job scheduler.
//!
//! Replaces the Flask APScheduler with a pure-Rust cron scheduler using `croner`.
//! Jobs are persisted in the `scheduled_job` table and reloaded on startup.
//! The nightly SMART check is also registered here (when enabled).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use croner::Cron;

use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::scheduler as repo;
use crate::db::Db;
use crate::error::AppError;
use crate::exec;
use crate::runners::smart_monitor::{self, Source};

type JobId = String;

/// A scheduled job with its cron expression and the function to run.
struct ScheduledJobEntry {
    id: JobId,
    cron: Cron,
    next_run: chrono::DateTime<Local>,
    func: Arc<dyn Fn() + Send + Sync>,
}

/// In-memory scheduler state.
struct SchedulerState {
    jobs: HashMap<JobId, ScheduledJobEntry>,
    running: bool,
}

/// Global scheduler state.
static SCHEDULER: Mutex<Option<Arc<Mutex<SchedulerState>>>> = Mutex::new(None);

/// Initialize the scheduler with jobs from the database.
pub fn init_scheduler(db: &Db, cfg: &Config) -> Result<(), AppError> {
    let state = Arc::new(Mutex::new(SchedulerState {
        jobs: HashMap::new(),
        running: false,
    }));

    // Load jobs from database
    let jobs = repo::list_jobs(db)?;
    for job in jobs {
        if job.enabled && job.cron.is_some() {
            let cron_expr = job.cron.clone().unwrap();
            let cron = crate::sched::parse_cron(&cron_expr)
                .map_err(|e| AppError::internal("cron parse", e))?;
            let next_run = crate::sched::next_run(&cron_expr, Local::now());
            
            let job_id = job.id.to_string();
            let job_id_for_closure = job_id.clone(); // Clone for the closure
            let job_id_for_entry = job_id.clone(); // Clone for the entry
            let db_clone = db.clone();
            let cfg_clone = cfg.clone();
            
            let func = Arc::new(move || {
                run_scheduled_job(&db_clone, &cfg_clone, &job_id_for_closure);
            });
            
            let entry = ScheduledJobEntry {
                id: job_id_for_entry,
                cron,
                next_run: next_run.unwrap_or_else(|| Local::now()),
                func,
            };
            state.lock().unwrap().jobs.insert(job_id, entry);
        }
    }

    // Register the nightly SMART check if enabled
    if cfg.enable_scheduler {
        let mut smart_cron = Cron::new("0 3 * * *");
        smart_cron.parse().unwrap();
        let next_run = smart_cron.find_next_occurrence(&Local::now(), false).unwrap();
        let db_clone = db.clone();
        let func = Arc::new(move || {
            let _ = smart_monitor::run_check(&db_clone, Source::Auto);
        });
        
        state.lock().unwrap().jobs.insert("smart_check".to_string(), ScheduledJobEntry {
            id: "smart_check".to_string(),
            cron: smart_cron,
            next_run,
            func,
        });
    }

    *SCHEDULER.lock().unwrap() = Some(state.clone());
    
    // Start the scheduler loop in a background task
    tokio::spawn(scheduler_loop(state));

    Ok(())
}

/// Register a new job with the running scheduler.
pub fn register_job(db: &Db, cfg: &Config, job_id: i64) -> Result<(), AppError> {
    if let Some(job) = repo::get_job(db, job_id)? {
        if job.enabled && job.cron.is_some() {
            let cron_expr = job.cron.clone().unwrap();
            let cron = crate::sched::parse_cron(&cron_expr)
                .map_err(|e| AppError::internal("cron parse", e))?;
            let next_run = crate::sched::next_run(&cron_expr, Local::now());
            
            let job_id_str = job.id.to_string();
            let job_id_for_closure = job_id_str.clone(); // Clone for the closure
            let job_id_for_entry = job_id_str.clone(); // Clone for the entry
            let db_clone = db.clone();
            let cfg_clone = cfg.clone();
            
            let func = Arc::new(move || {
                run_scheduled_job(&db_clone, &cfg_clone, &job_id_for_closure);
            });
            
            let entry = ScheduledJobEntry {
                id: job_id_for_entry,
                cron,
                next_run: next_run.unwrap_or_else(|| Local::now()),
                func,
            };
            
            if let Some(scheduler) = SCHEDULER.lock().unwrap().as_ref() {
                scheduler.lock().unwrap().jobs.insert(job_id_str, entry);
            }
        }
    }
    Ok(())
}

/// Main scheduler loop - checks every minute for jobs to run.
async fn scheduler_loop(state: Arc<Mutex<SchedulerState>>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        
        let now = Local::now();
        let mut to_run = Vec::new();
        
        {
            let mut state = state.lock().unwrap();
            for (id, entry) in state.jobs.iter_mut() {
                if let Ok(next) = entry.cron.find_next_occurrence(&entry.next_run, true) {
                    if next <= now {
                        to_run.push((id.clone(), entry.func.clone()));
                        entry.next_run = entry.cron.find_next_occurrence(&now, false).unwrap_or(now);
                    }
                }
            }
        }
        
        for (id, func) in to_run {
            tracing::info!("Running scheduled job: {}", id);
            func();
        }
    }
}

/// Execute a scheduled job and record the result.
fn run_scheduled_job(db: &Db, cfg: &Config, job_id: &str) {
    if let Ok(Some(job)) = repo::get_job(db, job_id.parse().unwrap_or(0)) {
        let log_path = cfg.runs_dir.join(format!("sched_{}.log", job_id));
        let _ = std::fs::create_dir_all(&log_path.parent().unwrap());
        
        let command = job.command.unwrap_or_default();
        let start = Instant::now();
        exec::log_write(&log_path, &format!("\n$ {}\n", command));
        
        let out = exec::run(
            &["bash", "-lc", &command],
            None,
            None,
            Duration::from_secs(3600),
        );
        
        exec::log_write(&log_path, &out.combined());
        let duration = start.elapsed();
        
        let status = if out.ok() { "success" } else { "failed" };
        let now_ms = crate::db::now_ms();
        
        let _ = repo::mark_job_run(db, job.id, status, now_ms);
        let _ = audit::record(db, "scheduler", "scheduled_job.run", Some(&job.name), 
            Some(&format!("duration={:?} rc={}", duration, out.rc)));
    }
}

/// Get the next run time for a job (for display).
pub fn get_next_run(db: &Db, job_id: i64) -> Option<chrono::DateTime<Local>> {
    if let Ok(Some(job)) = repo::get_job(db, job_id) {
        if let Some(cron_expr) = &job.cron {
            return crate::sched::next_run(cron_expr, Local::now());
        }
    }
    None
}

/// Public API to trigger a job manually.
pub fn trigger_job(db: &Db, cfg: &Config, job_id: i64) -> Result<(), AppError> {
    if let Some(job) = repo::get_job(db, job_id)? {
        run_scheduled_job(db, cfg, &job_id.to_string());
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_parsing_works() {
        assert!(crate::sched::parse_cron("0 3 * * *").is_ok());
        assert!(crate::sched::parse_cron("*/15 * * * *").is_ok());
    }
}