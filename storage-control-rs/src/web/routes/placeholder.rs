//! Placeholder pages for the sections that later phases implement.
//! They exist so the navigation works while the port is in progress.

use axum::response::Response;

use crate::error::AppError;
use crate::templates::PlaceholderPage;
use crate::web::session::Session;
use crate::web::render;

fn page(
    session: &Session,
    nav: &'static str,
    title: &'static str,
    note: &str,
) -> Result<Response, AppError> {
    session.require_uid()?;
    Ok(render(PlaceholderPage {
        nav,
        csrf: session.csrf(),
        flashes: session.flashes(),
        title,
        note: note.to_string(),
    }))
}

pub async fn services(session: Session) -> Result<Response, AppError> {
    page(
        &session,
        "services",
        "Services",
        "Раздел переносится в фазе 3: каталог юнитов, управление и журналы.",
    )
}

pub async fn backups(session: Session) -> Result<Response, AppError> {
    page(
        &session,
        "backups",
        "Backups",
        "Раздел переносится в фазе 4: задачи, запуски, файлы и retention.",
    )
}

pub async fn scheduler(session: Session) -> Result<Response, AppError> {
    page(
        &session,
        "scheduler",
        "Scheduler",
        "Раздел переносится в фазе 6: задачи, таймеры, cron и реестр джоб.",
    )
}

pub async fn updates(session: Session) -> Result<Response, AppError> {
    page(
        &session,
        "updates",
        "Updates",
        "Раздел переносится в фазе 5: версии, релизы, раннеры обновлений и preserve.",
    )
}
