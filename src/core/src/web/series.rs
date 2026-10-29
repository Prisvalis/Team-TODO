//! Recurring series: overview, editing, stopping and personal reminders.

use axum::extract::{Form, Path, State};

use super::forms::FormData;
use super::issues::{
    FormTarget, form_from_post, form_page, form_tz, offset_options, parse_recurrence,
    parse_reminder, rows, series_input, validate_issue,
};
use super::views::{IssueForm, LabelView, SeriesPage};
use super::{App, AppError, Ctx, FlashKind, HandlerResult, redirect_flash, render};
use crate::db::Runtime;
use crate::domain::{Reminder, Series};
use crate::ids::is_valid_id;
use crate::permission::{can_manage_series, can_view_series};
use crate::recurrence::format_date;
use crate::{schedule, store, time};

/// Upcoming occurrences cover the generation horizon; past ones are only a short history.
const UPCOMING_LIMIT: i64 = 40;
const PAST_LIMIT: i64 = 10;

/// Loads a series the user may see; anything else is a 404 so existence is not leaked.
async fn visible_series<R: Runtime>(app: &App<R>, ctx: &Ctx, id: &str) -> Result<Series, AppError> {
    if !is_valid_id(id) {
        return Err(AppError::NotFound);
    }
    match store::find_series(&app.rt, id).await? {
        Some(series) if can_view_series(&series, &ctx.user.id) => Ok(series),
        _ => Err(AppError::NotFound),
    }
}

async fn managed_series<R: Runtime>(app: &App<R>, ctx: &Ctx, id: &str) -> Result<Series, AppError> {
    let series = visible_series(app, ctx, id).await?;
    if !can_manage_series(&series, &ctx.user) {
        return Err(AppError::Forbidden);
    }
    Ok(series)
}

pub async fn show<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    let series = visible_series(&app, &ctx, &id).await?;
    let me = ctx.user.id.as_str();
    let upcoming = store::series_issues(&app.rt, &id, me, true, UPCOMING_LIMIT).await?;
    let past = store::series_issues(&app.rt, &id, me, false, PAST_LIMIT).await?;
    let reminder = store::find_series_reminder(&app.rt, &id, me).await?;
    let (reminder_mode, offset) = match reminder {
        Some(r) => (r.mode().0, r.mode().1.unwrap_or(60)),
        None => ("none", 60),
    };
    let mut period = format!("自 {} 起", format_date(series.rule.start_date));
    if let Some(until) = series.rule.until {
        period.push_str(&format!("，至 {}", format_date(until)));
    }
    if let Some(stopped) = series.stopped_at {
        period.push_str(&format!(
            "；已於 {} 停止",
            time::display(stopped, ctx.site.tz)
        ));
    }
    render(
        &SeriesPage {
            layout: ctx.layout(&series.title, "issues"),
            id: series.id.clone(),
            body: series.body.clone(),
            rule: series.rule.describe(),
            period,
            visibility_label: series.visibility.label(),
            issuer: series.issuer_name.clone(),
            assignee: series
                .assignee_name
                .clone()
                .filter(|_| series.assignee_active)
                .unwrap_or_default(),
            labels: series.labels.iter().map(LabelView::from).collect(),
            stopped: series.stopped_at.is_some(),
            can_manage: can_manage_series(&series, &ctx.user),
            upcoming: rows(&app, &ctx, &upcoming),
            past: rows(&app, &ctx, &past),
            back: format!("/series/{id}"),
            reminder_mode: reminder_mode.to_owned(),
            offsets: offset_options(&offset.to_string()),
        },
        ctx.had_flash(),
    )
}

fn form_from_series(series: &Series) -> IssueForm {
    let tz = series.timezone;
    let rule = &series.rule;
    // The rule's first occurrence, as the start and due fields of the form.
    let start = rule
        .start_ms(rule.start_date, tz)
        .unwrap_or(series.created_at);
    IssueForm {
        title: series.title.clone(),
        body: series.body.clone(),
        status: String::new(),
        visibility: series.visibility.as_str().into(),
        assignee_id: series.assignee_id.clone().unwrap_or_default(),
        start_at: time::input_value(start, tz),
        due_at: time::input_value(start + rule.duration_min * 60_000, tz),
        label_ids: series.labels.iter().map(|l| l.id.clone()).collect(),
        remind_mode: "none".into(),
        remind_at: String::new(),
        remind_offset: "60".into(),
        version: series.updated_at,
        conflict: false,
        tz: tz.name().to_owned(),
        repeat: rule.freq.as_str().into(),
        weekdays: (0..7)
            .filter(|i| rule.weekdays & (1 << i) != 0)
            .map(|i| i.to_string())
            .collect(),
        until: rule.until.map(format_date).unwrap_or_default(),
    }
}

fn stopped_redirect(id: &str) -> HandlerResult {
    Ok(redirect_flash(
        &format!("/series/{id}"),
        FlashKind::Err,
        "這個週期活動已停止",
    ))
}

pub async fn edit_page<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    let series = managed_series(&app, &ctx, &id).await?;
    if series.stopped_at.is_some() {
        return stopped_redirect(&id);
    }
    let form = form_from_series(&series);
    form_page(&app, &ctx, FormTarget::Series(&id), form, Vec::new()).await
}

pub async fn update<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let series = managed_series(&app, &ctx, &id).await?;
    if series.stopped_at.is_some() {
        return stopped_redirect(&id);
    }
    let form = form_from_post(&FormData(raw));
    let target = || FormTarget::Series(&id);
    let input = match validate_issue(
        &app,
        &form,
        ctx.site.tz,
        false,
        series.assignee_id.as_deref(),
    )
    .await?
    {
        Ok((input, _)) => input,
        Err(errors) => return form_page(&app, &ctx, target(), form, errors).await,
    };
    let tz = form_tz(&form, ctx.site.tz);
    let rule = match parse_recurrence(&form, input.start_at, input.due_at, tz) {
        Ok(Some(rule)) => rule,
        Ok(None) => {
            let errors = vec!["請選擇重複方式；不再重複請使用「停止重複」".to_owned()];
            return form_page(&app, &ctx, target(), form, errors).await;
        }
        Err(errors) => return form_page(&app, &ctx, target(), form, errors).await,
    };
    if !store::update_series(&app.rt, &id, form.version, series_input(input, rule, tz)).await? {
        let errors =
            vec!["這個週期活動在你編輯時已被其他人更新或停止，請重新開啟編輯頁".to_owned()];
        return form_page(&app, &ctx, target(), form, errors).await;
    }
    if let Some(series) = store::find_series(&app.rt, &id).await? {
        schedule::generate(&app.rt, &series, app.rt.now_ms()).await?;
    }
    Ok(redirect_flash(
        &format!("/series/{id}"),
        FlashKind::Ok,
        "週期活動已更新，尚未開始的場次已套用新設定",
    ))
}

pub async fn stop<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    managed_series(&app, &ctx, &id).await?;
    store::stop_series(&app.rt, &id).await?;
    Ok(redirect_flash(
        &format!("/series/{id}"),
        FlashKind::Ok,
        "已停止重複，尚未開始的場次已移除",
    ))
}

pub async fn set_reminder<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(raw);
    let series = visible_series(&app, &ctx, &id).await?;
    if series.stopped_at.is_some() {
        return stopped_redirect(&id);
    }
    let target = format!("/series/{id}");
    // Every occurrence has a start and a due time; only relative reminders apply.
    let parsed = match form.get("remind_mode") {
        "exact" => Err("週期活動的提醒請選「開始前」或「到期前」".to_owned()),
        mode => parse_reminder(
            mode,
            "",
            form.get("remind_offset"),
            Some(0),
            Some(0),
            ctx.site.tz,
        ),
    };
    match parsed {
        Ok(reminder) => {
            store::set_series_reminder(&app.rt, &id, &ctx.user.id, reminder).await?;
            let message = match reminder {
                Some(Reminder::BeforeStart(_) | Reminder::BeforeDue(_)) => {
                    "提醒已設定，會套用到每一場尚未開始的活動"
                }
                _ => "已取消這個週期活動的提醒",
            };
            Ok(redirect_flash(&target, FlashKind::Ok, message))
        }
        Err(e) => Ok(redirect_flash(&target, FlashKind::Err, &e)),
    }
}
