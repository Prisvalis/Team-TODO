//! JSON endpoints polled by `static/app.js` to show reminders.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::{App, AppError, Ctx, HandlerResult};
use crate::db::Runtime;
use crate::ids::is_valid_id;
use crate::{store, time};

pub async fn due_reminders<R: Runtime>(State(app): State<App<R>>, ctx: Ctx) -> HandlerResult {
    let now = app.rt.now_ms();
    let tz = ctx.site.tz;
    let reminders: Vec<_> = store::due_reminders(&app.rt, &ctx.user.id)
        .await?
        .into_iter()
        .map(|r| {
            let due = r
                .due_at
                .map(|d| {
                    format!(
                        "到期：{}（{}）",
                        time::display(d, tz),
                        time::relative(d, now)
                    )
                })
                .unwrap_or_else(|| "未設定到期時間".to_owned());
            json!({
                "issue_id": r.issue_id,
                "title": r.title,
                "url": format!("/issues/{}", r.issue_id),
                "remind_at": time::display(r.remind_at, tz),
                "detail": due,
            })
        })
        .collect();
    // Relative, so the page does not depend on its own clock being right.
    let next_in_ms = store::next_reminder_at(&app.rt, &ctx.user.id)
        .await?
        .map(|at| (at - app.rt.now_ms()).max(0));
    Ok(Json(json!({ "reminders": reminders, "next_in_ms": next_in_ms })).into_response())
}

pub async fn ack_reminder<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    if !is_valid_id(&id) {
        return Err(AppError::NotFound);
    }
    let first = store::ack_reminder(&app.rt, &id, &ctx.user.id).await?;
    Ok(if first {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CONFLICT
    }
    .into_response())
}
