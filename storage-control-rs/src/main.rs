use std::path::PathBuf;
use std::process::ExitCode;

use storage_control::config::Config;
use storage_control::web;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "storage_control=info,tower_http=warn".into()),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") | None => serve(&args).await,
        Some("version") | Some("--version") | Some("-V") => {
            println!("storage-control {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("import") => run_import(&args),
        Some("smart-check") => smart_check(&args),
        Some("retention") => not_yet("retention", "phase 4"),
        Some("-h") | Some("--help") => {
            usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

async fn serve(args: &[String]) -> ExitCode {
    let env_file = flag(args, "--env-file").map(PathBuf::from);
    let bind = flag(args, "--bind").unwrap_or_else(|| "0.0.0.0:8765".to_string());

    let cfg = match Config::load(env_file.as_deref()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };

    match web::serve(cfg, &bind).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `storage-control smart-check [--manual]` — the same check the nightly
/// scheduler runs, usable from cron or by hand.
fn smart_check(args: &[String]) -> ExitCode {
    use storage_control::db::Db;
    use storage_control::runners::smart_monitor::{self, Source};

    let env_file = flag(args, "--env-file").map(PathBuf::from);
    let cfg = match Config::load(env_file.as_deref()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let db = match Db::open(&cfg.db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("database error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source = if args.iter().any(|arg| arg == "--manual") {
        Source::Manual
    } else {
        Source::Auto
    };
    match smart_monitor::run_check(&db, source) {
        Ok(results) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".into())
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("smart-check failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `storage-control import --from PATH [--to PATH] [--force]`
fn run_import(args: &[String]) -> ExitCode {
    use storage_control::importer;

    let env_file = flag(args, "--env-file").map(PathBuf::from);
    let cfg = match Config::load(env_file.as_deref()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(from) = flag(args, "--from") else {
        eprintln!("import: --from PATH is required");
        return ExitCode::from(2);
    };
    let options = importer::Options {
        from: PathBuf::from(from),
        to: flag(args, "--to")
            .map(PathBuf::from)
            .unwrap_or_else(|| cfg.db_path.clone()),
        force: args.iter().any(|arg| arg == "--force"),
        backup_root: cfg.backup_root.clone(),
    };
    match importer::import(&options) {
        Ok(report) => {
            println!("{}", report.render());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn not_yet(command: &str, phase: &str) -> ExitCode {
    eprintln!("{command}: not implemented yet ({phase})");
    ExitCode::from(2)
}

fn usage() {
    println!(
        "storage-control {}\n\
         \n\
         Usage: storage-control <command> [options]\n\
         \n\
         Commands:\n\
           serve       [--bind ADDR] [--env-file PATH]   run the web console (default 0.0.0.0:8765)\n\
           import      --from PATH [--to PATH]           import the legacy Flask app.db\n\
           smart-check [--manual]                        run the SMART check and exit\n\
           retention                                     run backup retention cleanup and exit\n\
           version                                       print the version\n",
        env!("CARGO_PKG_VERSION")
    );
}
