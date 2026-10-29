//! Dashboard, issue lists, board, team workload and issue CRUD.

use std::collections::HashMap;

use axum::extract::{Form, Path, Query, State};
use chrono::Timelike;

use super::forms::FormData;
use super::views::{
    BoardColumn, BoardPage, DashboardPage, IssueForm, IssueFormPage, IssuePage, IssueRow,
    IssuesPage, LabelOption, Opt, ReminderRow, ReminderView, StatCard, Tab, TeamPage, TeamRow,
};
use super::{App, AppError, Ctx, FlashKind, HandlerResult, redirect_flash, render, safe_next};
use crate::db::Runtime;
use crate::domain::{BODY_MAX, Issue, Reminder, Status, TITLE_MAX, Visibility};
use crate::ids::is_valid_id;
use crate::permission::{
    can_change_status, can_claim, can_delete, can_manage, can_release, can_view,
    can_view_after_release,
};
use crate::recurrence::{Freq, Recurrence, WEEKDAY_LABELS, parse_date, weekday_bit};
use crate::store::{self, DueFilter, IssueFilter, IssueInput, Scope, SeriesInput, Sort};
use crate::{schedule, time};

type QueryMap = Query<HashMap<String, String>>;

fn param<'a>(q: &'a HashMap<String, String>, key: &str) -> &'a str {
    q.get(key).map_or("", |v| v.trim())
}

pub(super) fn rows<R: Runtime>(app: &App<R>, ctx: &Ctx, issues: &[Issue]) -> Vec<IssueRow> {
    let now = app.rt.now_ms();
    issues
        .iter()
        .map(|i| IssueRow::new(i, &ctx.user, ctx.site.tz, now))
        .collect()
}

pub(super) async fn assignee_options<R: Runtime>(
    app: &App<R>,
    current: &str,
    any_label: &str,
) -> Result<Vec<Opt>, AppError> {
    let mut options = vec![Opt::new("", any_label, current)];
    for user in store::list_users(&app.rt, false).await? {
        // Deactivated users stay selectable only where they already are the assignee,
        // so editing a finished issue does not silently erase who did it.
        if user.is_active {
            options.push(Opt::new(user.id, user.display_name, current));
        } else if user.id == current {
            options.push(Opt::new(
                user.id,
                format!("{}（已停用）", user.display_name),
                current,
            ));
        }
    }
    Ok(options)
}

/// The label filter's options and the selected id. Ids of deleted labels are ignored, so
/// a list never filters by something its dropdown cannot show.
async fn label_filter<R: Runtime>(
    app: &App<R>,
    current: &str,
) -> Result<(Option<String>, Vec<Opt>), AppError> {
    let labels = store::list_labels(&app.rt).await?;
    let selected = labels
        .iter()
        .any(|l| l.id == current)
        .then(|| current.to_owned());
    let mut options = vec![Opt::new("", "全部標籤", current)];
    options.extend(labels.into_iter().map(|l| Opt::new(l.id, l.name, current)));
    Ok((selected, options))
}

/// `none` selects unassigned issues; anything else must be a user id.
fn assignee_filter(key: &str) -> Option<String> {
    match key {
        "none" => Some(String::new()),
        id if is_valid_id(id) => Some(id.to_owned()),
        _ => None,
    }
}

/// Loads an issue the user may see; anything else is a 404 so existence is not leaked.
async fn visible_issue<R: Runtime>(app: &App<R>, ctx: &Ctx, id: &str) -> Result<Issue, AppError> {
    if !is_valid_id(id) {
        return Err(AppError::NotFound);
    }
    match store::find_issue(&app.rt, id).await? {
        Some(issue) if can_view(&issue, &ctx.user.id) => Ok(issue),
        _ => Err(AppError::NotFound),
    }
}

// ---------------------------------------------------------------- dashboard

pub async fn dashboard<R: Runtime>(State(app): State<App<R>>, ctx: Ctx) -> HandlerResult {
    let me = ctx.user.id.as_str();
    let stats = store::dashboard(&app.rt, me).await?;
    let mine = store::list_issues(
        &app.rt,
        me,
        &IssueFilter {
            due: DueFilter::Any,
            scope: Scope::Assigned,
            status: None,
            include_done: false,
            label_id: None,
            assignee_id: None,
            query: None,
            sort: Sort::Due,
            limit: 15,
        },
    )
    .await?;
    let created = store::list_issues(
        &app.rt,
        me,
        &IssueFilter {
            due: DueFilter::Any,
            scope: Scope::Created,
            status: None,
            include_done: false,
            label_id: None,
            assignee_id: None,
            query: None,
            sort: Sort::Updated,
            limit: 8,
        },
    )
    .await?;
    let now = app.rt.now_ms();
    let reminders = store::upcoming_reminders(&app.rt, me, 5)
        .await?
        .into_iter()
        .map(|r| ReminderRow {
            issue_id: r.issue_id,
            title: r.title,
            at_ms: r.remind_at,
            at: time::display(r.remind_at, ctx.site.tz),
            rel: time::relative(r.remind_at, now),
        })
        .collect();
    let cards = vec![
        StatCard {
            label: "指派給我（未完成）",
            value: stats.assigned_open,
            href: "/issues?scope=assigned".into(),
            tone: "",
        },
        StatCard {
            label: "已逾期",
            value: stats.overdue,
            href: "/issues?scope=assigned&due=overdue".into(),
            tone: if stats.overdue > 0 { "danger" } else { "" },
        },
        StatCard {
            label: "24 小時內到期",
            value: stats.due_soon,
            href: "/issues?scope=assigned&due=soon".into(),
            tone: if stats.due_soon > 0 { "warn" } else { "" },
        },
        StatCard {
            label: "可認領的公開工作",
            value: stats.claimable,
            href: "/issues?scope=public&assignee=none".into(),
            tone: "",
        },
    ];
    render(
        &DashboardPage {
            layout: ctx.layout("儀表板", "dashboard"),
            back: "/".to_owned(),
            cards,
            mine: rows(&app, &ctx, &mine),
            created: rows(&app, &ctx, &created),
            reminders,
        },
        ctx.had_flash(),
    )
}

// ---------------------------------------------------------------- list

pub async fn list<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Query(q): QueryMap,
) -> HandlerResult {
    let scope_key = match param(&q, "scope") {
        s @ ("created" | "public" | "all") => s,
        _ => "assigned",
    };
    let status_key = param(&q, "status");
    let label_key = param(&q, "label");
    let assignee_key = param(&q, "assignee");
    let sort_key = param(&q, "sort");
    let due_key = param(&q, "due");
    // Long keywords only cost a scan; nobody types more than this.
    let query: String = param(&q, "q").chars().take(100).collect();
    let query = query.as_str();
    let (label_id, label_options) = label_filter(&app, label_key).await?;

    let filter = IssueFilter {
        due: DueFilter::parse(due_key),
        scope: Scope::parse(scope_key),
        status: Status::parse(status_key),
        include_done: status_key == "any",
        label_id,
        assignee_id: assignee_filter(assignee_key),
        query: Some(query.to_owned()).filter(|v| !v.is_empty()),
        sort: Sort::parse(sort_key),
        limit: 200,
    };
    let issues = store::list_issues(&app.rt, &ctx.user.id, &filter).await?;

    let tabs = [
        ("assigned", "指派給我"),
        ("created", "我開立的"),
        ("public", "公開"),
        ("all", "全部可見"),
    ]
    .into_iter()
    .map(|(key, label)| Tab {
        href: format!("/issues?scope={key}"),
        label,
        active: key == scope_key,
    })
    .collect();
    let mut statuses = vec![
        Opt::new("", "未完成", status_key),
        Opt::new("any", "全部狀態", status_key),
    ];
    statuses.extend(Status::ALL.map(|s| Opt::new(s.as_str(), s.label(), status_key)));
    let mut assignees = assignee_options(&app, assignee_key, "全部負責人").await?;
    assignees.insert(1, Opt::new("none", "未指派", assignee_key));
    let dues = vec![
        Opt::new("", "全部到期時間", due_key),
        Opt::new("overdue", "已逾期", due_key),
        Opt::new("soon", "24 小時內到期", due_key),
    ];
    let back = {
        let mut url = form_urlencoded::Serializer::new(String::new());
        for key in ["scope", "status", "label", "assignee", "due", "sort", "q"] {
            let value = param(&q, key);
            if !value.is_empty() {
                url.append_pair(key, value);
            }
        }
        format!("/issues?{}", url.finish())
    };
    let sorts = vec![
        Opt::new("due", "依到期時間", sort_key),
        Opt::new("updated", "依最近更新", sort_key),
        Opt::new("created", "依建立時間", sort_key),
    ];

    render(
        &IssuesPage {
            layout: ctx.layout("工作清單", "issues"),
            dues,
            back,
            tabs,
            scope: scope_key.to_owned(),
            rows: rows(&app, &ctx, &issues),
            statuses,
            labels: label_options,
            assignees,
            sorts,
            q: query.to_owned(),
            filtered: !(status_key.is_empty()
                && due_key.is_empty()
                && label_key.is_empty()
                && assignee_key.is_empty()
                && query.is_empty()),
        },
        ctx.had_flash(),
    )
}

// ---------------------------------------------------------------- board

pub async fn board<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Query(q): QueryMap,
) -> HandlerResult {
    let scope_key = match param(&q, "scope") {
        s @ ("assigned" | "created" | "public") => s,
        _ => "all",
    };
    let label_key = param(&q, "label");
    let assignee_key = param(&q, "assignee");
    let (label_id, label_options) = label_filter(&app, label_key).await?;
    let assignee_id = assignee_filter(assignee_key);
    // Filters carried into links, so they survive actions and "view all".
    let mut filters = format!("scope={scope_key}");
    if let Some(label) = &label_id {
        filters.push_str(&format!("&label={label}"));
    }
    match assignee_id.as_deref() {
        Some("") => filters.push_str("&assignee=none"),
        Some(id) => filters.push_str(&format!("&assignee={id}")),
        None => {}
    }
    let mut columns = Vec::new();
    for status in Status::ALL {
        let issues = store::list_issues(
            &app.rt,
            &ctx.user.id,
            &IssueFilter {
                due: DueFilter::Any,
                scope: Scope::parse(scope_key),
                status: Some(status),
                include_done: true,
                label_id: label_id.clone(),
                assignee_id: assignee_id.clone(),
                query: None,
                sort: if status == Status::Done {
                    Sort::Updated
                } else {
                    Sort::Due
                },
                // One extra row tells whether the column was cut off.
                limit: board_limit(status) + 1,
            },
        )
        .await?;
        let more = issues.len() > board_limit(status) as usize;
        let shown = &issues[..issues.len().min(board_limit(status) as usize)];
        columns.push(BoardColumn {
            status: status.as_str(),
            label: status.label(),
            more,
            list_href: format!("/issues?{filters}&status={}", status.as_str()),
            rows: rows(&app, &ctx, shown),
            moves: Status::ALL
                .into_iter()
                .filter(|s| *s != status)
                .map(|s| (s.as_str(), s.label()))
                .collect(),
        });
    }
    let scopes = vec![
        Opt::new("all", "全部可見", scope_key),
        Opt::new("assigned", "指派給我", scope_key),
        Opt::new("created", "我開立的", scope_key),
        Opt::new("public", "公開", scope_key),
    ];
    let mut assignees = assignee_options(&app, assignee_key, "全部負責人").await?;
    assignees.insert(1, Opt::new("none", "未指派", assignee_key));
    render(
        &BoardPage {
            layout: ctx.layout("看板", "board"),
            columns,
            scopes,
            labels: label_options,
            assignees,
            back: format!("/board?{filters}"),
        },
        ctx.had_flash(),
    )
}

// ---------------------------------------------------------------- team

pub async fn team<R: Runtime>(State(app): State<App<R>>, ctx: Ctx) -> HandlerResult {
    let viewer = (!ctx.user.is_admin).then_some(ctx.user.id.as_str());
    let (loads, claimable) = store::workload(&app.rt, viewer).await?;
    let busiest = loads.iter().map(|w| w.active()).max().unwrap_or(0).max(1);
    let rows = loads
        .iter()
        .map(|w| TeamRow {
            user_id: w.user_id.clone(),
            name: w.display_name.clone(),
            open: w.open,
            in_progress: w.in_progress,
            overdue: w.overdue,
            due_week: w.due_week,
            open_pct: w.open * 100 / busiest,
            progress_pct: w.in_progress * 100 / busiest,
        })
        .collect();
    render(
        &TeamPage {
            layout: ctx.layout("團隊負載", "team"),
            rows,
            claimable,
            counts_all: ctx.user.is_admin,
        },
        ctx.had_flash(),
    )
}

// ---------------------------------------------------------------- create / edit

fn empty_form(ctx: &Ctx, assignee: &str) -> IssueForm {
    IssueForm {
        title: String::new(),
        body: String::new(),
        status: Status::Open.as_str().into(),
        visibility: Visibility::Private.as_str().into(),
        assignee_id: if is_valid_id(assignee) {
            assignee.to_owned()
        } else {
            ctx.user.id.clone()
        },
        start_at: String::new(),
        due_at: String::new(),
        label_ids: Vec::new(),
        remind_mode: "none".into(),
        remind_at: String::new(),
        remind_offset: "60".into(),
        version: 0,
        conflict: false,
        tz: String::new(),
        repeat: "none".into(),
        weekdays: Vec::new(),
        until: String::new(),
    }
}

fn board_limit(status: Status) -> i64 {
    if status == Status::Done { 30 } else { 100 }
}

fn form_from_issue(tz: chrono_tz::Tz, issue: &Issue) -> IssueForm {
    IssueForm {
        title: issue.title.clone(),
        body: issue.body.clone(),
        status: issue.status.as_str().into(),
        visibility: issue.visibility.as_str().into(),
        assignee_id: issue.assignee_id.clone().unwrap_or_default(),
        start_at: issue
            .start_at
            .map(|t| time::input_value(t, tz))
            .unwrap_or_default(),
        due_at: issue
            .due_at
            .map(|t| time::input_value(t, tz))
            .unwrap_or_default(),
        label_ids: issue.labels.iter().map(|l| l.id.clone()).collect(),
        remind_mode: "none".into(),
        remind_at: String::new(),
        remind_offset: "60".into(),
        version: issue.updated_at,
        conflict: false,
        tz: tz.name().to_owned(),
        repeat: "none".into(),
        weekdays: Vec::new(),
        until: String::new(),
    }
}

pub(super) fn form_from_post(form: &FormData) -> IssueForm {
    IssueForm {
        title: form.get("title").to_owned(),
        // Browsers submit textarea line breaks as CRLF but count them as one character.
        body: form.get("body").replace("\r\n", "\n"),
        status: form.get("status").to_owned(),
        visibility: form.get("visibility").to_owned(),
        assignee_id: form.get("assignee_id").to_owned(),
        start_at: form.get("start_at").to_owned(),
        due_at: form.get("due_at").to_owned(),
        label_ids: form.all("labels"),
        remind_mode: form.get("remind_mode").to_owned(),
        remind_at: form.get("remind_at").to_owned(),
        remind_offset: form.get("remind_offset").to_owned(),
        version: form.get("version").parse().unwrap_or(0),
        conflict: false,
        tz: form.get("tz").to_owned(),
        repeat: form.get("repeat").to_owned(),
        weekdays: form.all("weekdays"),
        until: form.get("until").to_owned(),
    }
}

pub(super) fn offset_options(current: &str) -> Vec<Opt> {
    Reminder::allowed_offsets()
        .into_iter()
        .map(|(minutes, label)| Opt::new(minutes.to_string(), label, current))
        .collect()
}

/// Parses the reminder fields shared by the new-issue form and the issue page.
pub(super) fn parse_reminder(
    mode: &str,
    at: &str,
    offset: &str,
    start_at: Option<i64>,
    due_at: Option<i64>,
    tz: chrono_tz::Tz,
) -> Result<Option<Reminder>, String> {
    let offset_minutes = || -> Result<i64, String> {
        let minutes: i64 = offset
            .parse()
            .map_err(|_| "請選擇提前多久提醒".to_owned())?;
        if Reminder::is_allowed_offset(minutes) {
            Ok(minutes)
        } else {
            Err("提醒時間選項不正確".into())
        }
    };
    match mode {
        "exact" => match time::parse_input(at, tz) {
            Ok(Some(t)) => Ok(Some(Reminder::Exact(t))),
            Ok(None) => Err("請選擇提醒時間".into()),
            Err(e) => Err(format!("提醒時間：{e}")),
        },
        "before_start" => {
            let minutes = offset_minutes()?;
            if start_at.is_none() {
                return Err("「開始前提醒」需要先設定開始時間".into());
            }
            Ok(Some(Reminder::BeforeStart(minutes)))
        }
        "before_due" => {
            let minutes = offset_minutes()?;
            if due_at.is_none() {
                return Err("「到期前提醒」需要先設定到期時間".into());
            }
            Ok(Some(Reminder::BeforeDue(minutes)))
        }
        _ => Ok(None),
    }
}

type Validated = Result<(IssueInput, Option<Reminder>), Vec<String>>;

/// Times are read in the zone the form was shown in, so an admin changing the site
/// zone meanwhile does not shift them.
pub(super) fn form_tz(form: &IssueForm, site_tz: chrono_tz::Tz) -> chrono_tz::Tz {
    form.tz.parse().unwrap_or(site_tz)
}

/// Longest single occurrence of a series.
const MAX_OCCURRENCE_MIN: i64 = 7 * 24 * 60;

/// Reads the repeat fields. The start time is the first occurrence and the span from
/// start to due time is the length of every occurrence.
pub(super) fn parse_recurrence(
    form: &IssueForm,
    start_at: Option<i64>,
    due_at: Option<i64>,
    tz: chrono_tz::Tz,
) -> Result<Option<Recurrence>, Vec<String>> {
    let Some(freq) = Freq::parse(&form.repeat) else {
        return Ok(None);
    };
    let (Some(start), Some(due)) = (start_at, due_at) else {
        return Err(vec![
            "週期活動需要設定開始與到期時間（第一場的時段）".to_owned(),
        ]);
    };
    let mut errors = Vec::new();
    let duration_min = (due - start) / 60_000;
    if duration_min > MAX_OCCURRENCE_MIN {
        errors.push("週期活動每場最長 7 天".to_owned());
    }
    let local = chrono::DateTime::from_timestamp_millis(start)
        .unwrap_or_default()
        .with_timezone(&tz);
    let start_date = local.date_naive();
    let weekdays = match freq {
        Freq::Weekly => {
            let bits = form
                .weekdays
                .iter()
                .filter_map(|d| d.parse::<u8>().ok())
                .filter(|d| *d < 7)
                .fold(0u8, |bits, d| bits | 1 << d);
            // Nothing checked means the weekday of the first occurrence.
            if bits == 0 {
                weekday_bit(start_date)
            } else {
                bits
            }
        }
        Freq::Daily | Freq::Monthly => 0,
    };
    let until = match form.until.trim() {
        "" => None,
        value => match parse_date(value) {
            Some(date) if date >= start_date => Some(date),
            Some(_) => {
                errors.push("結束日期不可早於第一場".to_owned());
                None
            }
            None => {
                errors.push("結束日期格式不正確".to_owned());
                None
            }
        },
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Some(Recurrence {
        freq,
        weekdays,
        start_date,
        until,
        start_minute: local.hour() * 60 + local.minute(),
        duration_min,
    }))
}

pub(super) fn series_input(
    input: IssueInput,
    rule: Recurrence,
    timezone: chrono_tz::Tz,
) -> SeriesInput {
    SeriesInput {
        title: input.title,
        body: input.body,
        visibility: input.visibility,
        assignee_id: input.assignee_id,
        label_ids: input.label_ids,
        rule,
        timezone,
    }
}

/// `current_assignee` may stay assigned even if deactivated (see `assignee_options`).
pub(super) async fn validate_issue<R: Runtime>(
    app: &App<R>,
    form: &IssueForm,
    site_tz: chrono_tz::Tz,
    with_reminder: bool,
    current_assignee: Option<&str>,
) -> Result<Validated, AppError> {
    let tz = form_tz(form, site_tz);
    let mut errors = Vec::new();
    let title = form.title.trim().to_owned();
    if title.is_empty() {
        errors.push("請輸入標題".to_owned());
    } else if title.chars().count() > TITLE_MAX {
        errors.push(format!("標題最多 {TITLE_MAX} 個字元"));
    }
    if form.body.chars().count() > BODY_MAX {
        errors.push(format!("備註最多 {BODY_MAX} 個字元"));
    }
    let status = Status::parse(&form.status).unwrap_or(Status::Open);
    let visibility = Visibility::parse(&form.visibility).unwrap_or(Visibility::Private);
    let assignee_id = match form.assignee_id.as_str() {
        "" => None,
        id => {
            // The current assignee may stay even if deactivated: finished work keeps its
            // history, and store::update_issue releases them when the issue is reopened.
            let allowed = Some(id) == current_assignee
                || (is_valid_id(id)
                    && store::find_user(&app.rt, id)
                        .await?
                        .is_some_and(|u| u.is_active));
            if !allowed {
                errors.push("指派對象不存在或已停用".to_owned());
            }
            Some(id.to_owned())
        }
    };
    let mut parse_time = |value: &str, name: &str| match time::parse_input(value, tz) {
        Ok(t) => t,
        Err(e) => {
            errors.push(format!("{name}：{e}"));
            None
        }
    };
    let start_at = parse_time(&form.start_at, "開始時間");
    let due_at = parse_time(&form.due_at, "到期時間");
    if let (Some(start), Some(due)) = (start_at, due_at)
        && start > due
    {
        errors.push("開始時間不可晚於到期時間".to_owned());
    }
    let reminder = if with_reminder {
        parse_reminder(
            &form.remind_mode,
            &form.remind_at,
            &form.remind_offset,
            start_at,
            due_at,
            tz,
        )
        .unwrap_or_else(|e| {
            errors.push(e);
            None
        })
    } else {
        None
    };
    if !errors.is_empty() {
        return Ok(Err(errors));
    }
    Ok(Ok((
        IssueInput {
            title,
            body: form.body.trim_end().to_owned(),
            status,
            visibility,
            assignee_id,
            start_at,
            due_at,
            label_ids: form
                .label_ids
                .iter()
                .filter(|id| is_valid_id(id))
                .cloned()
                .collect(),
        },
        reminder,
    )))
}

/// What the issue form edits.
pub(super) enum FormTarget<'a> {
    New,
    /// An issue; `in_series` when it is one occurrence of a series.
    Issue {
        id: &'a str,
        in_series: bool,
    },
    Series(&'a str),
}

pub(super) async fn form_page<R: Runtime>(
    app: &App<R>,
    ctx: &Ctx,
    target: FormTarget<'_>,
    form: IssueForm,
    errors: Vec<String>,
) -> HandlerResult {
    let labels = store::list_labels(&app.rt)
        .await?
        .into_iter()
        .map(|l| LabelOption {
            checked: form.label_ids.contains(&l.id),
            id: l.id,
            name: l.name,
            color: l.color,
        })
        .collect();
    let statuses = Status::ALL
        .map(|s| Opt::new(s.as_str(), s.label(), &form.status))
        .into_iter()
        .collect();
    let (title, action, cancel) = match target {
        FormTarget::Issue { id, .. } => (
            "編輯工作",
            format!("/issues/{id}/edit"),
            format!("/issues/{id}"),
        ),
        FormTarget::Series(id) => (
            "編輯週期活動",
            format!("/series/{id}/edit"),
            format!("/series/{id}"),
        ),
        FormTarget::New => ("新增工作", "/issues/new".to_owned(), "/issues".to_owned()),
    };
    let freqs = Freq::ALL
        .map(|f| Opt::new(f.as_str(), f.label(), &form.repeat))
        .into_iter()
        .collect();
    let weekday_opts = WEEKDAY_LABELS
        .iter()
        .enumerate()
        .map(|(i, label)| Opt {
            selected: form.weekdays.contains(&i.to_string()),
            value: i.to_string(),
            label: (*label).to_owned(),
        })
        .collect();
    render(
        &IssueFormPage {
            layout: ctx.layout(title, "issues"),
            is_edit: !matches!(target, FormTarget::New),
            is_series: matches!(target, FormTarget::Series(_)),
            in_series: matches!(
                target,
                FormTarget::Issue {
                    in_series: true,
                    ..
                }
            ),
            freqs,
            weekday_opts,
            action,
            cancel,
            assignees: assignee_options(app, &form.assignee_id, "不指派").await?,
            offsets: offset_options(&form.remind_offset),
            statuses,
            labels,
            form,
            errors,
        },
        ctx.had_flash(),
    )
}

pub async fn new_page<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Query(q): QueryMap,
) -> HandlerResult {
    let form = empty_form(&ctx, param(&q, "assignee"));
    form_page(&app, &ctx, FormTarget::New, form, Vec::new()).await
}

pub async fn create<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = form_from_post(&FormData(raw));
    let (input, reminder) = match validate_issue(&app, &form, ctx.site.tz, true, None).await? {
        Ok(valid) => valid,
        Err(errors) => return form_page(&app, &ctx, FormTarget::New, form, errors).await,
    };
    let tz = form_tz(&form, ctx.site.tz);
    let rule = match parse_recurrence(&form, input.start_at, input.due_at, tz) {
        Ok(Some(_)) if matches!(reminder, Some(Reminder::Exact(_))) => {
            let errors = vec!["週期活動的提醒請選「開始前」或「到期前」".to_owned()];
            return form_page(&app, &ctx, FormTarget::New, form, errors).await;
        }
        Ok(rule) => rule,
        Err(errors) => return form_page(&app, &ctx, FormTarget::New, form, errors).await,
    };
    let Some(rule) = rule else {
        let id = store::create_issue(&app.rt, &ctx.user.id, input, reminder).await?;
        return Ok(redirect_flash(
            &format!("/issues/{id}"),
            FlashKind::Ok,
            "工作已建立",
        ));
    };
    let id = store::create_series(
        &app.rt,
        &ctx.user.id,
        series_input(input, rule, tz),
        reminder,
    )
    .await?;
    // Show the first occurrences right away instead of on a later request.
    if let Some(series) = store::find_series(&app.rt, &id).await? {
        schedule::generate(&app.rt, &series, app.rt.now_ms()).await?;
    }
    Ok(redirect_flash(
        &format!("/series/{id}"),
        FlashKind::Ok,
        "週期活動已建立",
    ))
}

pub async fn edit_page<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    let issue = visible_issue(&app, &ctx, &id).await?;
    if !can_manage(&issue, &ctx.user) {
        return Err(AppError::Forbidden);
    }
    let form = form_from_issue(ctx.site.tz, &issue);
    let target = FormTarget::Issue {
        id: &id,
        in_series: issue.series_id.is_some(),
    };
    form_page(&app, &ctx, target, form, Vec::new()).await
}

pub async fn update<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let issue = visible_issue(&app, &ctx, &id).await?;
    if !can_manage(&issue, &ctx.user) {
        return Err(AppError::Forbidden);
    }
    let mut form = form_from_post(&FormData(raw));
    let target = || FormTarget::Issue {
        id: &id,
        in_series: issue.series_id.is_some(),
    };
    match validate_issue(
        &app,
        &form,
        ctx.site.tz,
        false,
        issue.assignee_id.as_deref(),
    )
    .await?
    {
        Ok((input, _)) => {
            if store::update_issue(&app.rt, &id, form.version, input).await? {
                return Ok(redirect_flash(
                    &format!("/issues/{id}"),
                    FlashKind::Ok,
                    "工作已更新",
                ));
            }
            // Someone claimed, released or moved it meanwhile: keep the user's text edits
            // but take over the fields other people can change, then ask to save again.
            let latest = store::find_issue(&app.rt, &id)
                .await?
                .ok_or(AppError::NotFound)?;
            form.status = latest.status.as_str().into();
            form.assignee_id = latest.assignee_id.clone().unwrap_or_default();
            form.version = latest.updated_at;
            form.conflict = true;
            form_page(&app, &ctx, target(), form, Vec::new()).await
        }
        Err(errors) => form_page(&app, &ctx, target(), form, errors).await,
    }
}

// ---------------------------------------------------------------- detail and actions

pub async fn show<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    let issue = visible_issue(&app, &ctx, &id).await?;
    let me = ctx.user.id.as_str();
    let tz = ctx.site.tz;
    let now = app.rt.now_ms();
    let reminder = match store::find_reminder(&app.rt, &id, me).await? {
        Some((r, fire_at, notified)) => ReminderView {
            past: fire_at <= now,
            mode: r.mode().0.into(),
            at: match r {
                Reminder::Exact(t) => time::input_value(t, tz),
                _ => String::new(),
            },
            offset: r.mode().1.unwrap_or(60).to_string(),
            fire_text: time::display(fire_at, tz),
            fire_rel: time::relative(fire_at, now),
            fire_ms: fire_at,
            notified,
        },
        None => ReminderView {
            past: false,
            mode: "none".into(),
            at: String::new(),
            offset: "60".into(),
            fire_text: String::new(),
            fire_rel: String::new(),
            fire_ms: 0,
            notified: false,
        },
    };
    let statuses = Status::ALL
        .map(|s| Opt::new(s.as_str(), s.label(), issue.status.as_str()))
        .into_iter()
        .collect();
    render(
        &IssuePage {
            layout: ctx.layout(&issue.title, "issues"),
            body: issue.body.clone(),
            visibility_label: issue.visibility.label(),
            start: issue
                .start_at
                .map(|t| time::display(t, tz))
                .unwrap_or_default(),
            created: time::display(issue.created_at, tz),
            can_manage: can_manage(&issue, &ctx.user),
            can_delete: can_delete(&issue, &ctx.user),
            release_hides: !can_view_after_release(&issue, me),
            can_status: can_change_status(&issue, &ctx.user),
            can_claim: can_claim(&issue, me),
            can_release: can_release(&issue, me),
            statuses,
            offsets: offset_options(&reminder.offset),
            reminder,
            has_start: issue.start_at.is_some(),
            has_due: issue.due_at.is_some(),
            series_id: issue.series_id.clone().unwrap_or_default(),
            row: IssueRow::new(&issue, &ctx.user, tz, now),
        },
        ctx.had_flash(),
    )
}

fn back_to(form: &FormData, id: &str) -> String {
    safe_next(form.get("next"), &format!("/issues/{id}"))
}

pub async fn change_status<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(raw);
    let issue = visible_issue(&app, &ctx, &id).await?;
    if !can_change_status(&issue, &ctx.user) {
        return Err(AppError::Forbidden);
    }
    let status = Status::parse(form.get("status")).ok_or(AppError::NotFound)?;
    store::set_status(&app.rt, &id, status).await?;
    Ok(redirect_flash(
        &back_to(&form, &id),
        FlashKind::Ok,
        &format!("「{}」已改為{}", issue.title, status.label()),
    ))
}

pub async fn claim<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(raw);
    let issue = visible_issue(&app, &ctx, &id).await?;
    if !can_claim(&issue, &ctx.user.id) {
        let reason = if issue.status == Status::Done {
            "這項工作已完成，不需要認領"
        } else {
            "這項工作已經有負責人"
        };
        return Ok(redirect_flash(&back_to(&form, &id), FlashKind::Err, reason));
    }
    if store::claim_issue(&app.rt, &id, &ctx.user.id).await? {
        Ok(redirect_flash(
            &back_to(&form, &id),
            FlashKind::Ok,
            "已認領這項工作",
        ))
    } else {
        Ok(redirect_flash(
            &back_to(&form, &id),
            FlashKind::Err,
            "這項工作剛被其他人認領或已完成",
        ))
    }
}

pub async fn release<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    let issue = visible_issue(&app, &ctx, &id).await?;
    if !can_release(&issue, &ctx.user.id) {
        return Err(AppError::Forbidden);
    }
    store::release_issue(&app.rt, &id, &ctx.user.id).await?;
    // A private issue is no longer visible after releasing it.
    let target = if can_view_after_release(&issue, &ctx.user.id) {
        format!("/issues/{id}")
    } else {
        "/issues".to_owned()
    };
    Ok(redirect_flash(
        &target,
        FlashKind::Ok,
        "已放棄這項工作的指派",
    ))
}

pub async fn set_reminder<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(raw);
    let issue = visible_issue(&app, &ctx, &id).await?;
    let target = format!("/issues/{id}");
    if issue.status == Status::Done {
        return Ok(redirect_flash(
            &target,
            FlashKind::Err,
            "這項工作已完成，不能設定提醒",
        ));
    }
    match parse_reminder(
        form.get("remind_mode"),
        form.get("remind_at"),
        form.get("remind_offset"),
        issue.start_at,
        issue.due_at,
        ctx.site.tz,
    ) {
        Ok(reminder) => {
            store::set_reminder(
                &app.rt,
                &id,
                &ctx.user.id,
                reminder,
                issue.start_at,
                issue.due_at,
            )
            .await?;
            let message = if reminder.is_some() {
                "提醒已設定"
            } else {
                "提醒已取消"
            };
            Ok(redirect_flash(&target, FlashKind::Ok, message))
        }
        Err(e) => Ok(redirect_flash(&target, FlashKind::Err, &e)),
    }
}

pub async fn delete<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
) -> HandlerResult {
    let issue = visible_issue(&app, &ctx, &id).await?;
    if !can_delete(&issue, &ctx.user) {
        return Err(AppError::Forbidden);
    }
    store::delete_issue(&app.rt, &id).await?;
    let target = if issue.issuer_id == ctx.user.id {
        "/issues?scope=created"
    } else {
        "/issues?scope=all"
    };
    Ok(redirect_flash(target, FlashKind::Ok, "工作已刪除"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: chrono_tz::Tz = chrono_tz::Asia::Taipei;

    #[test]
    fn reminder_parsing_rules() {
        assert_eq!(parse_reminder("none", "", "", None, None, TZ), Ok(None));
        assert!(parse_reminder("exact", "", "", None, None, TZ).is_err());
        assert!(matches!(
            parse_reminder("exact", "2026-10-24T09:00", "", None, None, TZ),
            Ok(Some(Reminder::Exact(_)))
        ));
        assert!(parse_reminder("before_due", "", "60", Some(1), None, TZ).is_err());
        assert!(parse_reminder("before_due", "", "90", None, Some(1), TZ).is_err());
        assert_eq!(
            parse_reminder("before_due", "", "1440", None, Some(1), TZ),
            Ok(Some(Reminder::BeforeDue(1440)))
        );
        assert!(parse_reminder("before_start", "", "60", None, Some(1), TZ).is_err());
        assert!(parse_reminder("before_start", "", "7", Some(1), None, TZ).is_ok());
        assert!(parse_reminder("before_start", "", "420", Some(1), None, TZ).is_err());
        assert_eq!(
            parse_reminder("before_start", "", "30", Some(1), None, TZ),
            Ok(Some(Reminder::BeforeStart(30)))
        );
    }
}
