//! Askama template structs.
//!
//! Every page receives the same three base-context fields (`nav`, `csrf`,
//! `flashes`) because `base.html` renders the header, the logout form and the
//! flash stack. That is the whole replacement for the Flask globals
//! `url_for` / `csrf_token()` / `get_flashed_messages()`.

use askama::Template;

use crate::web::session::Flash;

#[derive(Template)]
#[template(path = "auth/login.html")]
pub struct LoginPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
    pub username: String,
    pub next: String,
}

#[derive(Template)]
#[template(path = "dashboard/index.html")]
pub struct DashboardPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
}

#[derive(Template)]
#[template(path = "help.html")]
pub struct HelpPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
    pub readme_html: String,
}

#[derive(Template)]
#[template(path = "services/index.html")]
pub struct ServicesPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
    pub groups: Vec<ServiceGroupBlock>,
}
pub struct ServiceGroupBlock {
    pub name: String,
    pub label: String,
    pub rows: Vec<ServiceRow>,
}
pub struct ServiceRow {
    pub name: String,
    pub scope: String,
    pub desc: String,
    pub status: String,
    pub badge: &'static str,
    pub oneshot: bool,
    pub last_run: String,
    pub enabled: String,
    pub critical: bool,
    pub actions: Vec<ServiceAction>,
}

/// One control button: the action name and the CSS class of its button.

pub struct ServiceAction {
    pub name: &'static str,
    pub class: &'static str,
}

pub const SERVICE_ACTIONS: [(&str, &str); 5] = [
    ("start", "ok"),
    ("stop", "danger"),
    ("restart", ""),
    ("enable", ""),
    ("disable", ""),
];

#[derive(Template)]
#[template(path = "backups/index.html")]
pub struct BackupsPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
    pub task_groups: Vec<TaskGroup>,
    pub run_groups: Vec<RunGroup>,
    pub file_groups: Vec<FileGroup>,
    pub kind_options: Vec<KindOption>,
}
pub struct KindOption {
    pub value: &'static str,
    pub selected: bool,
}
pub struct TaskGroup {
    pub label: String,
    pub count: usize,
    pub tasks: Vec<TaskRow>,
}
pub struct TaskRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub cron: String,
    pub enabled: bool,
    pub retention_days: i64,
    pub command: String,
    pub last_run: String,
    pub last_status: String,
    pub kind_options: Vec<KindOption>,
}
pub struct RunGroup {
    pub label: String,
    pub count: usize,
    pub runs: Vec<BackupRunRow>,
}
pub struct BackupRunRow {
    pub id: i64,
    pub task: String,
    pub status: String,
    pub badge: &'static str,
    pub started: String,
    pub files: String,
}
pub struct FileGroup {
    pub label: String,
    pub count: usize,
    pub files: Vec<FileRow>,
}
pub struct FileRow {
    pub rel: String,
    pub size: String,
    pub mtime: String,
    pub is_dump: bool,
}

/// Updates page data.
///
/// NOTE: Simplified to avoid askama derive macro bug with multiple Vec fields.
/// The full updates page with preserves and runs is rendered manually.
/// See `web/routes/updates.rs` for the full implementation.
#[derive(Template)]
#[template(path = "updates/index.html")]
pub struct UpdatesPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
}

pub struct UpdateGroup {
    pub label: String,
    pub key: String,
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub error: Option<String>,
    pub release_url: Option<String>,
    pub changelog: String,
    pub migration_steps_json: String,
    pub tags_json: String,
}

impl UpdateGroup {
    pub fn display_latest(&self) -> &str {
        if self.latest.is_empty() { &self.current } else { &self.latest }
    }

    pub fn release_url_or_empty(&self) -> &str {
        self.release_url.as_deref().unwrap_or("")
    }

    pub fn migration_steps(&self) -> Vec<String> {
        serde_json::from_str(&self.migration_steps_json).unwrap_or_default()
    }

    pub fn tags(&self) -> Vec<String> {
        serde_json::from_str(&self.tags_json).unwrap_or_default()
    }
}

pub struct PreserveGroup {
    pub label: String,
    pub count: usize,
    pub preserves: Vec<PreserveRow>,
}

pub struct PreserveRow {
    pub id: i64,
    pub target: String,
    pub from_version: String,
    pub status: String,
    pub badge: &'static str,
    pub created: String,
    pub finished: String,
}

pub struct UpdateRunRow {
    pub id: i64,
    pub target: String,
    pub from_version: String,
    pub to_version: String,
    pub status: String,
    pub badge: &'static str,
    pub started: String,
}

#[derive(Template)]
#[template(path = "scheduler/index.html")]
pub struct SchedulerPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
    pub jobs: Vec<JobRow>,
    pub timers: Vec<TimerRow>,
    pub aps_jobs: Vec<APSJobRow>,
}

pub struct JobRow {
    pub id: i64,
    pub name: String,
    pub cron: String,
    pub enabled: bool,
    pub last_run: String,
    pub last_status: String,
}

pub struct TimerRow {
    pub scope: String,
    pub unit: String,
    pub raw: String,
}

pub struct APSJobRow {
    pub id: String,
    pub trigger: String,
    pub next: String,
}

#[derive(Template)]
#[template(path = "errors/404.html")]
pub struct NotFoundPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
}

#[derive(Template)]
#[template(path = "placeholder.html")]
pub struct PlaceholderPage {
    pub nav: &'static str,
    pub csrf: String,
    pub flashes: Vec<Flash>,
    pub title: &'static str,
    pub note: String,
}
