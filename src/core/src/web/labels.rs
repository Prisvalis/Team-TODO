use axum::extract::{Form, Path, State};

use super::forms::{self, FormData};
use super::views::{LabelRow, LabelsPage};
use super::{App, AppError, Ctx, FlashKind, HandlerResult, redirect_flash, render};
use crate::db::{DbError, Runtime};
use crate::ids::is_valid_id;
use crate::permission::can_delete_label;
use crate::store;

pub async fn page<R: Runtime>(State(app): State<App<R>>, ctx: Ctx) -> HandlerResult {
    let labels = store::list_labels(&app.rt)
        .await?
        .into_iter()
        .map(|l| LabelRow {
            can_delete: can_delete_label(&l, &ctx.user),
            id: l.id,
            name: l.name,
            color: l.color,
            is_system: l.is_system,
        })
        .collect();
    render(
        &LabelsPage {
            layout: ctx.layout("標籤", "labels"),
            labels,
        },
        ctx.had_flash(),
    )
}

pub async fn create<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(raw);
    let parsed = forms::label_name(form.get("name"))
        .and_then(|name| Ok((name, forms::color(form.get("color"))?)));
    let (name, color) = match parsed {
        Ok(values) => values,
        Err(e) => return Ok(redirect_flash("/labels", FlashKind::Err, e)),
    };
    match store::create_label(&app.rt, &name, &color, &ctx.user.id).await {
        Ok(()) => Ok(redirect_flash(
            "/labels",
            FlashKind::Ok,
            &format!("已新增標籤「{name}」"),
        )),
        Err(DbError::UniqueViolation) => {
            Ok(redirect_flash("/labels", FlashKind::Err, "已有同名的標籤"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn delete<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    if !is_valid_id(&id) {
        return Err(AppError::NotFound);
    }
    let label = store::find_label(&app.rt, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !can_delete_label(&label, &ctx.user) {
        return Err(AppError::Forbidden);
    }
    store::delete_label(&app.rt, &id).await?;
    Ok(redirect_flash(
        "/labels",
        FlashKind::Ok,
        &format!("已刪除標籤「{}」", label.name),
    ))
}
