//! Every SQL statement of the application. Only syntax that both MariaDB and
//! SQLite (D1) accept is allowed here; see docs/rule/README.md.

use std::collections::{BTreeSet, HashMap};

use crate::db::{DbError, Runtime, Stmt, Value};
use chrono::NaiveDate;

use crate::domain::{Issue, Label, Reminder, Series, Status, User, Visibility};
use crate::ids::{new_id, token_key};
use crate::permission::VISIBLE_TO;
use crate::recurrence::{Recurrence, format_date};

type Result<T> = std::result::Result<T, DbError>;

const USER_COLUMNS: &str =
    "id, username, display_name, email, is_admin, is_active, must_change_password";

const ISSUE_SELECT: &str = "SELECT i.id, i.title, i.body, i.status, i.visibility, i.issuer_id, \
     i.assignee_id, i.start_at, i.due_at, i.created_at, i.updated_at, i.series_id, \
     u1.display_name AS issuer_name, u1.is_active AS issuer_active, \
     u2.display_name AS assignee_name, u2.is_active AS assignee_active \
     FROM issues i JOIN users u1 ON u1.id = i.issuer_id LEFT JOIN users u2 ON u2.id = i.assignee_id";

const SESSION_TTL_MS: i64 = 7 * 24 * 3_600_000;

/// D1 rejects statements with more than 100 bound parameters.
const MAX_IN_LIST: usize = 90;

pub async fn ping<R: Runtime>(rt: &R) -> Result<()> {
    rt.query("SELECT 1 AS ok", vec![]).await.map(drop)
}

// ---------------------------------------------------------------- users

pub struct LoginRecord {
    pub user: User,
    pub password_hash: String,
    /// Unix milliseconds until which logins are refused after repeated failures.
    pub locked_until: Option<i64>,
}

pub async fn find_login<R: Runtime>(rt: &R, username: &str) -> Result<Option<LoginRecord>> {
    let rows = rt
        .query(
            &format!(
                "SELECT {USER_COLUMNS}, password_hash, locked_until FROM users WHERE username = ?"
            ),
            vec![username.into()],
        )
        .await?;
    Ok(rows.first().map(|row| LoginRecord {
        user: User::from_row(row),
        password_hash: row.text("password_hash"),
        locked_until: row.opt_int("locked_until"),
    }))
}

pub const MAX_FAILED_LOGINS: i64 = 5;
pub const LOGIN_LOCK_MS: i64 = 10 * 60_000;

/// Counts a wrong password; the fifth in a row locks the account for `LOGIN_LOCK_MS`.
/// `locked_until` is assigned first because MariaDB evaluates SET clauses left to right
/// with already-updated values, while SQLite uses the old row; both see the old count here.
pub async fn record_failed_login<R: Runtime>(rt: &R, user_id: &str) -> Result<()> {
    rt.execute(
        "UPDATE users SET \
         locked_until = CASE WHEN failed_logins + 1 >= ? THEN ? ELSE locked_until END, \
         failed_logins = CASE WHEN failed_logins + 1 >= ? THEN 0 ELSE failed_logins + 1 END \
         WHERE id = ?",
        vec![
            MAX_FAILED_LOGINS.into(),
            (rt.now_ms() + LOGIN_LOCK_MS).into(),
            MAX_FAILED_LOGINS.into(),
            user_id.into(),
        ],
    )
    .await
    .map(drop)
}

pub async fn clear_failed_logins<R: Runtime>(rt: &R, user_id: &str) -> Result<()> {
    rt.execute(
        "UPDATE users SET failed_logins = 0, locked_until = NULL WHERE id = ?",
        vec![user_id.into()],
    )
    .await
    .map(drop)
}

pub async fn password_hash<R: Runtime>(rt: &R, user_id: &str) -> Result<String> {
    let rows = rt
        .query(
            "SELECT password_hash FROM users WHERE id = ?",
            vec![user_id.into()],
        )
        .await?;
    Ok(rows
        .first()
        .map(|r| r.text("password_hash"))
        .unwrap_or_default())
}

pub async fn find_user<R: Runtime>(rt: &R, id: &str) -> Result<Option<User>> {
    let rows = rt
        .query(
            &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?"),
            vec![id.into()],
        )
        .await?;
    Ok(rows.first().map(User::from_row))
}

pub async fn list_users<R: Runtime>(rt: &R, active_only: bool) -> Result<Vec<User>> {
    let filter = if active_only {
        "WHERE is_active = 1"
    } else {
        ""
    };
    let rows = rt
        .query(
            &format!(
                "SELECT {USER_COLUMNS} FROM users {filter} ORDER BY is_active DESC, display_name"
            ),
            vec![],
        )
        .await?;
    Ok(rows.iter().map(User::from_row).collect())
}

pub struct NewUser<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub email: Option<&'a str>,
    pub password_hash: String,
    pub is_admin: bool,
    pub must_change_password: bool,
}

pub async fn create_user<R: Runtime>(rt: &R, user: NewUser<'_>) -> Result<String> {
    let id = new_id(rt);
    rt.execute(
        "INSERT INTO users (id, username, display_name, email, password_hash, is_admin, is_active, \
         must_change_password, created_at) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)",
        vec![
            id.as_str().into(),
            user.username.into(),
            user.display_name.into(),
            user.email.into(),
            user.password_hash.into(),
            user.is_admin.into(),
            user.must_change_password.into(),
            rt.now_ms().into(),
        ],
    )
    .await?;
    Ok(id)
}

/// Also ends every other session of the user, so a reset password takes effect everywhere.
pub async fn set_password<R: Runtime>(
    rt: &R,
    user_id: &str,
    hash: String,
    must_change: bool,
    keep_session: Option<&str>,
) -> Result<()> {
    rt.batch(vec![
        Stmt::new(
            "UPDATE users SET password_hash = ?, must_change_password = ? WHERE id = ?",
            vec![hash.into(), must_change.into(), user_id.into()],
        ),
        Stmt::new(
            "DELETE FROM sessions WHERE user_id = ? AND token <> ?",
            vec![
                user_id.into(),
                keep_session.map(token_key).unwrap_or_default().into(),
            ],
        ),
    ])
    .await
}

pub async fn set_admin<R: Runtime>(rt: &R, user_id: &str, is_admin: bool) -> Result<()> {
    rt.execute(
        "UPDATE users SET is_admin = ? WHERE id = ?",
        vec![is_admin.into(), user_id.into()],
    )
    .await
    .map(drop)
}

/// Deactivating also signs the user out and releases their unfinished work so the
/// team can pick it up instead of it sitting with someone who cannot log in.
pub async fn set_active<R: Runtime>(rt: &R, user_id: &str, is_active: bool) -> Result<()> {
    let mut stmts = vec![Stmt::new(
        "UPDATE users SET is_active = ? WHERE id = ?",
        vec![is_active.into(), user_id.into()],
    )];
    if !is_active {
        stmts.push(Stmt::new(
            "DELETE FROM sessions WHERE user_id = ?",
            vec![user_id.into()],
        ));
        stmts.push(Stmt::new(
            "UPDATE issues SET assignee_id = NULL, updated_at = ? \
             WHERE assignee_id = ? AND status <> 'done'",
            vec![rt.now_ms().into(), user_id.into()],
        ));
    }
    rt.batch(stmts).await
}

pub async fn set_profile<R: Runtime>(
    rt: &R,
    user_id: &str,
    display_name: &str,
    email: Option<&str>,
) -> Result<()> {
    rt.execute(
        "UPDATE users SET display_name = ?, email = ? WHERE id = ?",
        vec![display_name.into(), email.into(), user_id.into()],
    )
    .await
    .map(drop)
}

// ---------------------------------------------------------------- sessions

pub async fn create_session<R: Runtime>(rt: &R, token: &str, user_id: &str) -> Result<()> {
    let now = rt.now_ms();
    rt.batch(vec![
        Stmt::new(
            "DELETE FROM sessions WHERE expires_at < ?",
            vec![now.into()],
        ),
        Stmt::new(
            "INSERT INTO sessions (token, user_id, expires_at) VALUES (?, ?, ?)",
            vec![
                token_key(token).into(),
                user_id.into(),
                (now + SESSION_TTL_MS).into(),
            ],
        ),
    ])
    .await
}

pub async fn session_user<R: Runtime>(rt: &R, token: &str) -> Result<Option<User>> {
    let rows = rt
        .query(
            "SELECT u.id, u.username, u.display_name, u.email, u.is_admin, u.is_active, u.must_change_password \
             FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token = ? AND s.expires_at > ? AND u.is_active = 1",
            vec![token_key(token).into(), rt.now_ms().into()],
        )
        .await?;
    Ok(rows.first().map(User::from_row))
}

pub async fn delete_session<R: Runtime>(rt: &R, token: &str) -> Result<()> {
    rt.execute(
        "DELETE FROM sessions WHERE token = ?",
        vec![token_key(token).into()],
    )
    .await
    .map(drop)
}

// ---------------------------------------------------------------- settings

/// Raw `settings` rows; empty values mean "use the deployment's initial value".
#[derive(Debug, Clone, Default)]
pub struct StoredSettings {
    pub site_name: String,
    pub site_url: String,
    pub timezone: String,
    pub allow_signup: bool,
    pub setup_completed: bool,
}

pub async fn load_settings<R: Runtime>(rt: &R) -> Result<StoredSettings> {
    let rows = rt.query("SELECT name, value FROM settings", vec![]).await?;
    // A missing setup flag must not trap the site in setup mode (fail open to "done").
    let mut settings = StoredSettings {
        setup_completed: true,
        ..StoredSettings::default()
    };
    for row in &rows {
        let value = row.text("value");
        match row.text("name").as_str() {
            "site_name" => settings.site_name = value,
            "site_url" => settings.site_url = value,
            "timezone" => settings.timezone = value,
            "allow_signup" => settings.allow_signup = value == "1",
            "setup_completed" => settings.setup_completed = value == "1",
            _ => {}
        }
    }
    Ok(settings)
}

/// Site-wide values edited on the setup page and in the admin back office.
pub struct SiteUpdate {
    pub site_name: String,
    pub site_url: String,
    pub timezone: String,
    pub allow_signup: bool,
}

fn setting_stmt(name: &str, value: &str) -> Stmt {
    Stmt::new(
        "UPDATE settings SET value = ? WHERE name = ?",
        vec![value.into(), name.into()],
    )
}

fn site_stmts(site: &SiteUpdate) -> Vec<Stmt> {
    vec![
        setting_stmt("site_name", &site.site_name),
        setting_stmt("site_url", &site.site_url),
        setting_stmt("timezone", &site.timezone),
        setting_stmt("allow_signup", if site.allow_signup { "1" } else { "0" }),
    ]
}

pub async fn save_settings<R: Runtime>(rt: &R, site: &SiteUpdate) -> Result<()> {
    rt.batch(site_stmts(site)).await
}

pub struct AdminSetup {
    pub admin_id: String,
    pub display_name: String,
    pub email: String,
    pub password_hash: String,
    /// The session finishing setup; every other session of the account is ended.
    pub keep_session: String,
}

/// Finishes first-run setup in one atomic batch: site values, the administrator's
/// profile and password, ending other sessions, and finally the setup flag.
///
/// Every statement is guarded so a concurrent second submission changes nothing: until
/// this batch runs, the seeded administrator still has `must_change_password = 1`. (The
/// guards read `users` because MariaDB rejects a subquery on the table being updated.)
/// Returns false when someone else finished setup first.
pub async fn complete_setup<R: Runtime>(
    rt: &R,
    site: &SiteUpdate,
    admin: AdminSetup,
) -> Result<bool> {
    const ADMIN_PENDING: &str =
        "EXISTS (SELECT 1 FROM users WHERE id = ? AND must_change_password = 1)";
    let id = admin.admin_id.as_str();
    let mut stmts: Vec<Stmt> = site_stmts(site)
        .into_iter()
        .map(|mut s| {
            s.sql = format!("{} AND {ADMIN_PENDING}", s.sql);
            s.params.push(id.into());
            s
        })
        .collect();
    // Anyone else who logged in with the published default password is signed out.
    stmts.push(Stmt::new(
        format!("DELETE FROM sessions WHERE user_id = ? AND token <> ? AND {ADMIN_PENDING}"),
        vec![id.into(), token_key(&admin.keep_session).into(), id.into()],
    ));
    stmts.push(Stmt::new(
        "UPDATE users SET display_name = ?, email = ?, password_hash = ?, must_change_password = 0 \
         WHERE id = ? AND must_change_password = 1 \
         AND EXISTS (SELECT 1 FROM settings WHERE name = 'setup_completed' AND value = '0')",
        vec![
            admin.display_name.into(),
            admin.email.into(),
            admin.password_hash.as_str().into(),
            id.into(),
        ],
    ));
    stmts.push(Stmt::new(
        "UPDATE settings SET value = '1' WHERE name = 'setup_completed' AND value = '0' \
         AND EXISTS (SELECT 1 FROM users WHERE id = ? AND password_hash = ?)",
        vec![id.into(), admin.password_hash.as_str().into()],
    ));
    rt.batch(stmts).await?;
    // Our hash is in place only if this batch (not a concurrent one) did the work.
    Ok(password_hash(rt, id).await? == admin.password_hash)
}

// ---------------------------------------------------------------- labels

pub async fn list_labels<R: Runtime>(rt: &R) -> Result<Vec<Label>> {
    let rows = rt
        .query(
            "SELECT id, name, color, is_system, created_by FROM labels ORDER BY is_system DESC, created_at, id",
            vec![],
        )
        .await?;
    Ok(rows.iter().map(Label::from_row).collect())
}

pub async fn find_label<R: Runtime>(rt: &R, id: &str) -> Result<Option<Label>> {
    let rows = rt
        .query(
            "SELECT id, name, color, is_system, created_by FROM labels WHERE id = ?",
            vec![id.into()],
        )
        .await?;
    Ok(rows.first().map(Label::from_row))
}

pub async fn create_label<R: Runtime>(
    rt: &R,
    name: &str,
    color: &str,
    creator: &str,
) -> Result<()> {
    rt.execute(
        "INSERT INTO labels (id, name, color, is_system, created_by, created_at) VALUES (?, ?, ?, 0, ?, ?)",
        vec![
            new_id(rt).into(),
            name.into(),
            color.into(),
            creator.into(),
            rt.now_ms().into(),
        ],
    )
    .await
    .map(drop)
}

pub async fn delete_label<R: Runtime>(rt: &R, id: &str) -> Result<()> {
    rt.execute("DELETE FROM labels WHERE id = ?", vec![id.into()])
        .await
        .map(drop)
}

/// Keeps only ids that exist, so a forged form value cannot break the batch.
async fn existing_label_ids<R: Runtime>(rt: &R, ids: &[String]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let known: Vec<String> = list_labels(rt).await?.into_iter().map(|l| l.id).collect();
    // A set also drops repeated values, which would violate the issue_labels primary key.
    let wanted: BTreeSet<&String> = ids.iter().filter(|id| known.contains(id)).collect();
    Ok(wanted.into_iter().cloned().collect())
}

async fn attach_labels<R: Runtime>(rt: &R, issues: &mut [Issue]) -> Result<()> {
    if issues.is_empty() {
        return Ok(());
    }
    let mut by_issue: HashMap<String, Vec<Label>> = HashMap::new();
    for chunk in issues.chunks(MAX_IN_LIST) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let rows = rt
            .query(
                &format!(
                    "SELECT il.issue_id, l.id, l.name, l.color, l.is_system, l.created_by \
                     FROM issue_labels il JOIN labels l ON l.id = il.label_id \
                     WHERE il.issue_id IN ({placeholders}) ORDER BY l.is_system DESC, l.created_at, l.id"
                ),
                chunk.iter().map(|i| i.id.as_str().into()).collect(),
            )
            .await?;
        for row in &rows {
            by_issue
                .entry(row.text("issue_id"))
                .or_default()
                .push(Label::from_row(row));
        }
    }
    for issue in issues {
        issue.labels = by_issue.remove(&issue.id).unwrap_or_default();
    }
    Ok(())
}

// ---------------------------------------------------------------- issues

pub struct IssueInput {
    pub title: String,
    pub body: String,
    pub status: Status,
    pub visibility: Visibility,
    pub assignee_id: Option<String>,
    pub start_at: Option<i64>,
    pub due_at: Option<i64>,
    pub label_ids: Vec<String>,
}

/// Lowercased in Rust so MariaDB and SQLite (whose LOWER/LIKE only fold ASCII) search alike.
fn search_text(title: &str, body: &str) -> String {
    format!("{title}\n{body}").to_lowercase()
}

/// Reopened work goes back to the pool if its assignee was deactivated meanwhile.
fn release_inactive_assignee(issue_id: &str) -> Stmt {
    Stmt::new(
        "UPDATE issues SET assignee_id = NULL WHERE id = ? AND status <> 'done' \
         AND assignee_id IN (SELECT u.id FROM users u WHERE u.is_active = 0)",
        vec![issue_id.into()],
    )
}

/// Reopening revives reminders that finishing cancelled and that are still in the future.
fn revive_reminders(issue_id: &str, now: i64) -> Stmt {
    Stmt::new(
        "UPDATE reminders SET notified_at = NULL WHERE issue_id = ? AND remind_at > ?",
        vec![issue_id.into(), now.into()],
    )
}

fn retire_reminders(issue_id: &str, now: i64) -> Stmt {
    Stmt::new(
        "UPDATE reminders SET notified_at = ? WHERE issue_id = ? AND notified_at IS NULL",
        vec![now.into(), issue_id.into()],
    )
}

fn label_stmts(issue_id: &str, label_ids: &[String]) -> Vec<Stmt> {
    label_ids
        .iter()
        .map(|label_id| {
            Stmt::new(
                "INSERT INTO issue_labels (issue_id, label_id) VALUES (?, ?)",
                vec![issue_id.into(), label_id.as_str().into()],
            )
        })
        .collect()
}

pub async fn create_issue<R: Runtime>(
    rt: &R,
    issuer_id: &str,
    input: IssueInput,
    reminder: Option<Reminder>,
) -> Result<String> {
    let id = new_id(rt);
    let now = rt.now_ms();
    let label_ids = existing_label_ids(rt, &input.label_ids).await?;
    let mut stmts = vec![Stmt::new(
        "INSERT INTO issues (id, title, body, search_text, status, visibility, issuer_id, assignee_id, \
         start_at, due_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            id.as_str().into(),
            input.title.as_str().into(),
            input.body.as_str().into(),
            search_text(&input.title, &input.body).into(),
            input.status.as_str().into(),
            input.visibility.as_str().into(),
            issuer_id.into(),
            input.assignee_id.into(),
            input.start_at.into(),
            input.due_at.into(),
            now.into(),
            now.into(),
        ],
    )];
    stmts.extend(label_stmts(&id, &label_ids));
    if let Some(stmt) =
        reminder.and_then(|r| reminder_stmt(&id, issuer_id, r, input.start_at, input.due_at))
    {
        stmts.push(stmt);
    }
    rt.batch(stmts).await?;
    Ok(id)
}

/// Saves the edit form only if nobody changed the issue since `version` (its
/// `updated_at` when the form was opened); returns false on such a conflict.
pub async fn update_issue<R: Runtime>(
    rt: &R,
    id: &str,
    version: i64,
    input: IssueInput,
) -> Result<bool> {
    let label_ids = existing_label_ids(rt, &input.label_ids).await?;
    let now = rt.now_ms();
    let done = input.status == Status::Done;
    let saved = rt
        .execute(
            "UPDATE issues SET title = ?, body = ?, search_text = ?, status = ?, visibility = ?, \
             assignee_id = ?, start_at = ?, due_at = ?, updated_at = ? WHERE id = ? AND updated_at = ?",
            vec![
                input.title.as_str().into(),
                input.body.as_str().into(),
                search_text(&input.title, &input.body).into(),
                input.status.as_str().into(),
                input.visibility.as_str().into(),
                input.assignee_id.into(),
                input.start_at.into(),
                input.due_at.into(),
                // Never reuse the version the form carried, even within one millisecond.
                now.max(version.saturating_add(1)).into(),
                id.into(),
                version.into(),
            ],
        )
        .await?;
    if saved == 0 {
        return Ok(false);
    }
    let mut stmts = vec![Stmt::new(
        "DELETE FROM issue_labels WHERE issue_id = ?",
        vec![id.into()],
    )];
    stmts.extend(label_stmts(id, &label_ids));
    // Relative reminders follow their start or due time; without one they cannot fire.
    stmts.push(follow_anchor(id, "before_start", input.start_at));
    stmts.push(follow_anchor(id, "before_due", input.due_at));
    if done {
        stmts.push(retire_reminders(id, now));
    } else {
        stmts.push(release_inactive_assignee(id));
        stmts.push(revive_reminders(id, now));
    }
    rt.batch(stmts).await?;
    Ok(true)
}

/// Finishing retires pending reminders; reopening work held by a deactivated user
/// makes it unassigned so the team can claim it.
pub async fn set_status<R: Runtime>(rt: &R, id: &str, status: Status) -> Result<()> {
    let now = rt.now_ms();
    let stmts = if status == Status::Done {
        vec![
            Stmt::new(
                "UPDATE issues SET status = ?, updated_at = ? WHERE id = ?",
                vec![status.as_str().into(), now.into(), id.into()],
            ),
            retire_reminders(id, now),
        ]
    } else {
        vec![
            Stmt::new(
                "UPDATE issues SET status = ?, updated_at = ? WHERE id = ?",
                vec![status.as_str().into(), now.into(), id.into()],
            ),
            release_inactive_assignee(id),
            revive_reminders(id, now),
        ]
    };
    rt.batch(stmts).await
}

/// Returns false when someone else claimed it first.
pub async fn claim_issue<R: Runtime>(rt: &R, id: &str, user_id: &str) -> Result<bool> {
    let changed = rt
        .execute(
            "UPDATE issues SET assignee_id = ?, updated_at = ? \
             WHERE id = ? AND assignee_id IS NULL AND status <> 'done'",
            vec![user_id.into(), rt.now_ms().into(), id.into()],
        )
        .await?;
    Ok(changed > 0)
}

pub async fn release_issue<R: Runtime>(rt: &R, id: &str, user_id: &str) -> Result<()> {
    rt.execute(
        "UPDATE issues SET assignee_id = NULL, updated_at = ? WHERE id = ? AND assignee_id = ?",
        vec![rt.now_ms().into(), id.into(), user_id.into()],
    )
    .await
    .map(drop)
}

pub async fn delete_issue<R: Runtime>(rt: &R, id: &str) -> Result<()> {
    rt.execute("DELETE FROM issues WHERE id = ?", vec![id.into()])
        .await
        .map(drop)
}

pub async fn find_issue<R: Runtime>(rt: &R, id: &str) -> Result<Option<Issue>> {
    let rows = rt
        .query(&format!("{ISSUE_SELECT} WHERE i.id = ?"), vec![id.into()])
        .await?;
    let mut issues: Vec<Issue> = rows.iter().map(Issue::from_row).collect();
    attach_labels(rt, &mut issues).await?;
    Ok(issues.pop())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Assigned,
    Created,
    Public,
    All,
}

impl Scope {
    pub fn parse(s: &str) -> Self {
        match s {
            "created" => Scope::Created,
            "public" => Scope::Public,
            "all" => Scope::All,
            _ => Scope::Assigned,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Due,
    Updated,
    Created,
}

impl Sort {
    pub fn parse(s: &str) -> Self {
        match s {
            "updated" => Sort::Updated,
            "created" => Sort::Created,
            _ => Sort::Due,
        }
    }

    fn order_by(self) -> &'static str {
        match self {
            // Issues without a due time go last.
            Sort::Due => {
                "CASE WHEN i.due_at IS NULL THEN 1 ELSE 0 END, i.due_at, i.updated_at DESC"
            }
            Sort::Updated => "i.updated_at DESC",
            Sort::Created => "i.created_at DESC",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueFilter {
    Any,
    /// Unfinished and past the due time.
    Overdue,
    /// Unfinished and due within 24 hours.
    Soon,
}

impl DueFilter {
    pub fn parse(s: &str) -> Self {
        match s {
            "overdue" => DueFilter::Overdue,
            "soon" => DueFilter::Soon,
            _ => DueFilter::Any,
        }
    }
}

pub struct IssueFilter {
    pub scope: Scope,
    pub due: DueFilter,
    /// `None` hides finished issues; `Some` selects exactly one status.
    pub status: Option<Status>,
    pub include_done: bool,
    pub label_id: Option<String>,
    /// `Some("")` means unassigned.
    pub assignee_id: Option<String>,
    pub query: Option<String>,
    pub sort: Sort,
    pub limit: i64,
}

pub async fn list_issues<R: Runtime>(rt: &R, viewer: &str, f: &IssueFilter) -> Result<Vec<Issue>> {
    let mut sql = format!("{ISSUE_SELECT} WHERE {VISIBLE_TO}");
    let mut params: Vec<Value> = vec![viewer.into(), viewer.into()];
    match f.scope {
        Scope::Assigned => {
            sql.push_str(" AND i.assignee_id = ?");
            params.push(viewer.into());
        }
        Scope::Created => {
            sql.push_str(" AND i.issuer_id = ?");
            params.push(viewer.into());
        }
        Scope::Public => sql.push_str(" AND i.visibility = 'public'"),
        Scope::All => {}
    }
    match f.status {
        Some(status) => {
            sql.push_str(" AND i.status = ?");
            params.push(status.as_str().into());
        }
        None if !f.include_done => sql.push_str(" AND i.status <> 'done'"),
        None => {}
    }
    if let Some(label) = &f.label_id {
        sql.push_str(" AND EXISTS (SELECT 1 FROM issue_labels il WHERE il.issue_id = i.id AND il.label_id = ?)");
        params.push(label.as_str().into());
    }
    match f.assignee_id.as_deref() {
        Some("") => sql.push_str(" AND i.assignee_id IS NULL"),
        Some(assignee) => {
            sql.push_str(" AND i.assignee_id = ?");
            params.push(assignee.into());
        }
        None => {}
    }
    let now = rt.now_ms();
    match f.due {
        DueFilter::Overdue => {
            sql.push_str(" AND i.status <> 'done' AND i.due_at < ?");
            params.push(now.into());
        }
        DueFilter::Soon => {
            sql.push_str(" AND i.status <> 'done' AND i.due_at >= ? AND i.due_at < ?");
            params.push(now.into());
            params.push((now + 24 * 3_600_000).into());
        }
        DueFilter::Any => {}
    }
    if let Some(q) = f.query.as_deref().filter(|q| !q.is_empty()) {
        // INSTR on Rust-lowercased text: no LIKE wildcards/escapes and no D1 pattern limit.
        sql.push_str(" AND INSTR(i.search_text, ?) > 0");
        params.push(q.to_lowercase().into());
    }
    sql.push_str(&format!(
        " ORDER BY {} LIMIT {}",
        f.sort.order_by(),
        f.limit.clamp(1, 500)
    ));
    let rows = rt.query(&sql, params).await?;
    let mut issues: Vec<Issue> = rows.iter().map(Issue::from_row).collect();
    attach_labels(rt, &mut issues).await?;
    Ok(issues)
}

pub struct Dashboard {
    pub assigned_open: i64,
    pub overdue: i64,
    pub due_soon: i64,
    pub claimable: i64,
}

pub async fn dashboard<R: Runtime>(rt: &R, viewer: &str) -> Result<Dashboard> {
    let now = rt.now_ms();
    let rows = rt
        .query(
            "SELECT \
                 COUNT(CASE WHEN i.assignee_id = ? AND i.status <> 'done' THEN 1 END) AS assigned_open, \
                 COUNT(CASE WHEN i.assignee_id = ? AND i.status <> 'done' AND i.due_at < ? THEN 1 END) AS overdue, \
                 COUNT(CASE WHEN i.assignee_id = ? AND i.status <> 'done' AND i.due_at >= ? AND i.due_at < ? THEN 1 END) AS due_soon, \
                 COUNT(CASE WHEN i.assignee_id IS NULL AND i.status <> 'done' AND i.visibility = 'public' THEN 1 END) AS claimable \
                 FROM issues i WHERE i.status <> 'done' AND (i.assignee_id = ? OR \
                 (i.assignee_id IS NULL AND i.visibility = 'public'))",
            // Every counter needs an unfinished issue that is mine or claimable, so the
            // WHERE narrows the scan without changing any count (and implies visibility).
            vec![
                viewer.into(),
                viewer.into(),
                now.into(),
                viewer.into(),
                now.into(),
                (now + 24 * 3_600_000).into(),
                viewer.into(),
            ],
        )
        .await?;
    let row = rows.first().cloned().unwrap_or_default();
    Ok(Dashboard {
        assigned_open: row.int("assigned_open"),
        overdue: row.int("overdue"),
        due_soon: row.int("due_soon"),
        claimable: row.int("claimable"),
    })
}

pub struct Workload {
    pub user_id: String,
    pub display_name: String,
    pub open: i64,
    pub in_progress: i64,
    pub overdue: i64,
    pub due_week: i64,
}

impl Workload {
    pub fn active(&self) -> i64 {
        self.open + self.in_progress
    }
}

/// Per active member (administrators are not counted); `viewer: None` counts every issue
/// (admins), otherwise only visible ones.
pub async fn workload<R: Runtime>(rt: &R, viewer: Option<&str>) -> Result<(Vec<Workload>, i64)> {
    let now = rt.now_ms();
    let mut params: Vec<Value> = vec![now.into(), now.into(), (now + 7 * 24 * 3_600_000).into()];
    let visible = match viewer {
        Some(v) => {
            params.push(v.into());
            params.push(v.into());
            format!(" AND {VISIBLE_TO}")
        }
        None => String::new(),
    };
    let rows = rt
        .query(
            &format!(
                "SELECT u.id, u.display_name, \
                 COUNT(CASE WHEN i.status = 'open' THEN 1 END) AS open_count, \
                 COUNT(CASE WHEN i.status = 'in_progress' THEN 1 END) AS in_progress_count, \
                 COUNT(CASE WHEN i.status <> 'done' AND i.due_at < ? THEN 1 END) AS overdue_count, \
                 COUNT(CASE WHEN i.status <> 'done' AND i.due_at >= ? AND i.due_at < ? THEN 1 END) AS due_week_count \
                 FROM users u LEFT JOIN issues i ON i.assignee_id = u.id AND i.status <> 'done'{visible} \
                 WHERE u.is_active = 1 AND u.is_admin = 0 GROUP BY u.id, u.display_name ORDER BY u.display_name"
            ),
            params,
        )
        .await?;
    let mut list: Vec<Workload> = rows
        .iter()
        .map(|r| Workload {
            user_id: r.text("id"),
            display_name: r.text("display_name"),
            open: r.int("open_count"),
            in_progress: r.int("in_progress_count"),
            overdue: r.int("overdue_count"),
            due_week: r.int("due_week_count"),
        })
        .collect();
    list.sort_by_key(|w| std::cmp::Reverse((w.overdue, w.active())));

    // Same definition as the dashboard: public issues anyone may take over.
    let claimable = rt
        .query(
            "SELECT COUNT(*) AS n FROM issues i WHERE i.assignee_id IS NULL \
             AND i.status <> 'done' AND i.visibility = 'public'",
            vec![],
        )
        .await?
        .first()
        .map_or(0, |r| r.int("n"));
    Ok((list, claimable))
}

// ---------------------------------------------------------------- series

const SERIES_SELECT: &str = "SELECT s.id, s.title, s.body, s.visibility, s.issuer_id, s.assignee_id, \
     s.freq, s.weekdays, s.start_date, s.until_date, s.start_minute, s.duration_min, s.timezone, \
     s.effective_from, s.generated_until, s.stopped_at, s.created_at, s.updated_at, \
     u1.display_name AS issuer_name, u1.is_active AS issuer_active, \
     u2.display_name AS assignee_name, u2.is_active AS assignee_active \
     FROM series s JOIN users u1 ON u1.id = s.issuer_id LEFT JOIN users u2 ON u2.id = s.assignee_id";

/// Columns of one generated issue in the multi-row INSERT.
const OCCURRENCE_COLUMNS: usize = 14;

pub struct SeriesInput {
    pub title: String,
    pub body: String,
    pub visibility: Visibility,
    pub assignee_id: Option<String>,
    pub label_ids: Vec<String>,
    pub rule: Recurrence,
    pub timezone: chrono_tz::Tz,
}

fn series_label_stmts(series_id: &str, label_ids: &[String]) -> Vec<Stmt> {
    label_ids
        .iter()
        .map(|label_id| {
            Stmt::new(
                "INSERT INTO series_labels (series_id, label_id) VALUES (?, ?)",
                vec![series_id.into(), label_id.as_str().into()],
            )
        })
        .collect()
}

/// Only relative reminders make sense for a series; an exact time yields `None`.
fn series_reminder_stmt(series_id: &str, user_id: &str, reminder: Reminder) -> Option<Stmt> {
    let (mode, Some(offset)) = reminder.mode() else {
        return None;
    };
    Some(Stmt::new(
        "INSERT INTO series_reminders (series_id, user_id, mode, offset_min) VALUES (?, ?, ?, ?)",
        vec![series_id.into(), user_id.into(), mode.into(), offset.into()],
    ))
}

/// Not-yet-started occurrences nobody has picked up; edits and stopping replace them.
fn drop_future_occurrences(series_id: &str, now: i64) -> Stmt {
    Stmt::new(
        "DELETE FROM issues WHERE series_id = ? AND status = 'open' AND start_at > ?",
        vec![series_id.into(), now.into()],
    )
}

pub async fn create_series<R: Runtime>(
    rt: &R,
    issuer_id: &str,
    input: SeriesInput,
    reminder: Option<Reminder>,
) -> Result<String> {
    let id = new_id(rt);
    let now = rt.now_ms();
    let label_ids = existing_label_ids(rt, &input.label_ids).await?;
    let rule = &input.rule;
    let mut stmts = vec![Stmt::new(
        "INSERT INTO series (id, title, body, visibility, issuer_id, assignee_id, freq, weekdays, \
         start_date, until_date, start_minute, duration_min, timezone, effective_from, \
         generated_until, stopped_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?)",
        vec![
            id.as_str().into(),
            input.title.as_str().into(),
            input.body.as_str().into(),
            input.visibility.as_str().into(),
            issuer_id.into(),
            input.assignee_id.into(),
            rule.freq.as_str().into(),
            i64::from(rule.weekdays).into(),
            format_date(rule.start_date).into(),
            rule.until.map(format_date).into(),
            i64::from(rule.start_minute).into(),
            rule.duration_min.into(),
            input.timezone.name().into(),
            now.into(),
            now.into(),
            now.into(),
        ],
    )];
    stmts.extend(series_label_stmts(&id, &label_ids));
    if let Some(stmt) = reminder.and_then(|r| series_reminder_stmt(&id, issuer_id, r)) {
        stmts.push(stmt);
    }
    rt.batch(stmts).await?;
    Ok(id)
}

pub async fn find_series<R: Runtime>(rt: &R, id: &str) -> Result<Option<Series>> {
    let rows = rt
        .query(&format!("{SERIES_SELECT} WHERE s.id = ?"), vec![id.into()])
        .await?;
    let Some(mut series) = rows.first().map(Series::from_row) else {
        return Ok(None);
    };
    let labels = rt
        .query(
            "SELECT l.id, l.name, l.color, l.is_system, l.created_by \
             FROM series_labels sl JOIN labels l ON l.id = sl.label_id \
             WHERE sl.series_id = ? ORDER BY l.is_system DESC, l.created_at, l.id",
            vec![id.into()],
        )
        .await?;
    series.labels = labels.iter().map(Label::from_row).collect();
    Ok(Some(series))
}

/// Applies an edit to the occurrences that have not started yet; returns false when the
/// series changed since `version` (its `updated_at` when the form was opened) or was stopped.
pub async fn update_series<R: Runtime>(
    rt: &R,
    id: &str,
    version: i64,
    input: SeriesInput,
) -> Result<bool> {
    let label_ids = existing_label_ids(rt, &input.label_ids).await?;
    let now = rt.now_ms();
    let rule = &input.rule;
    let saved = rt
        .execute(
            "UPDATE series SET title = ?, body = ?, visibility = ?, assignee_id = ?, freq = ?, \
             weekdays = ?, start_date = ?, until_date = ?, start_minute = ?, duration_min = ?, \
             timezone = ?, effective_from = ?, generated_until = NULL, updated_at = ? \
             WHERE id = ? AND updated_at = ? AND stopped_at IS NULL",
            vec![
                input.title.as_str().into(),
                input.body.as_str().into(),
                input.visibility.as_str().into(),
                input.assignee_id.into(),
                rule.freq.as_str().into(),
                i64::from(rule.weekdays).into(),
                format_date(rule.start_date).into(),
                rule.until.map(format_date).into(),
                i64::from(rule.start_minute).into(),
                rule.duration_min.into(),
                input.timezone.name().into(),
                now.into(),
                now.max(version.saturating_add(1)).into(),
                id.into(),
                version.into(),
            ],
        )
        .await?;
    if saved == 0 {
        return Ok(false);
    }
    let mut stmts = vec![Stmt::new(
        "DELETE FROM series_labels WHERE series_id = ?",
        vec![id.into()],
    )];
    stmts.extend(series_label_stmts(id, &label_ids));
    stmts.push(drop_future_occurrences(id, now));
    rt.batch(stmts).await?;
    Ok(true)
}

/// Ends the series; past and picked-up occurrences stay as history.
pub async fn stop_series<R: Runtime>(rt: &R, id: &str) -> Result<()> {
    let now = rt.now_ms();
    rt.batch(vec![
        Stmt::new(
            "UPDATE series SET stopped_at = ?, updated_at = ? WHERE id = ? AND stopped_at IS NULL",
            vec![now.into(), now.into(), id.into()],
        ),
        drop_future_occurrences(id, now),
    ])
    .await
}

/// Occurrences of the series the viewer may see: upcoming ones soonest first, or past
/// ones latest first.
pub async fn series_issues<R: Runtime>(
    rt: &R,
    series_id: &str,
    viewer: &str,
    upcoming: bool,
    limit: i64,
) -> Result<Vec<Issue>> {
    let (cmp, order) = if upcoming {
        (">=", "ASC")
    } else {
        ("<", "DESC")
    };
    let rows = rt
        .query(
            &format!(
                "{ISSUE_SELECT} WHERE i.series_id = ? AND {VISIBLE_TO} AND i.start_at {cmp} ? \
                 ORDER BY i.start_at {order} LIMIT {}",
                limit.clamp(1, 100)
            ),
            vec![
                series_id.into(),
                viewer.into(),
                viewer.into(),
                rt.now_ms().into(),
            ],
        )
        .await?;
    let mut issues: Vec<Issue> = rows.iter().map(Issue::from_row).collect();
    attach_labels(rt, &mut issues).await?;
    Ok(issues)
}

pub async fn find_series_reminder<R: Runtime>(
    rt: &R,
    series_id: &str,
    user_id: &str,
) -> Result<Option<Reminder>> {
    let rows = rt
        .query(
            "SELECT mode, offset_min FROM series_reminders WHERE series_id = ? AND user_id = ?",
            vec![series_id.into(), user_id.into()],
        )
        .await?;
    Ok(rows.first().and_then(Reminder::from_row))
}

/// Fire time of a relative reminder on a generated occurrence, as SQL over `i` and `offset`.
fn occurrence_fire_sql(mode_column: &str, offset_column: &str) -> String {
    format!(
        "(CASE WHEN {mode_column} = 'before_start' THEN i.start_at ELSE i.due_at END - {offset_column} * 60000)"
    )
}

/// Replaces the user's series reminder and their reminders on occurrences that have not
/// started yet; `None` removes both.
pub async fn set_series_reminder<R: Runtime>(
    rt: &R,
    series_id: &str,
    user_id: &str,
    reminder: Option<Reminder>,
) -> Result<()> {
    let now = rt.now_ms();
    let mut stmts = vec![
        Stmt::new(
            "DELETE FROM series_reminders WHERE series_id = ? AND user_id = ?",
            vec![series_id.into(), user_id.into()],
        ),
        Stmt::new(
            "DELETE FROM reminders WHERE user_id = ? AND issue_id IN \
             (SELECT i.id FROM issues i WHERE i.series_id = ? AND i.status <> 'done' AND i.start_at > ?)",
            vec![user_id.into(), series_id.into(), now.into()],
        ),
    ];
    if let Some(reminder) = reminder
        && let Some(stmt) = series_reminder_stmt(series_id, user_id, reminder)
    {
        let (mode, offset) = reminder.mode();
        stmts.push(stmt);
        let fire = occurrence_fire_sql("?", "?");
        stmts.push(Stmt::new(
            format!(
                "INSERT INTO reminders (issue_id, user_id, mode, offset_min, remind_at, notified_at) \
                 SELECT i.id, ?, ?, ?, {fire}, NULL FROM issues i \
                 WHERE i.series_id = ? AND i.status <> 'done' AND i.start_at > ? AND {fire} > ?"
            ),
            vec![
                user_id.into(),
                mode.into(),
                offset.into(),
                mode.into(),
                offset.into(),
                series_id.into(),
                now.into(),
                mode.into(),
                offset.into(),
                now.into(),
            ],
        ));
    }
    rt.batch(stmts).await
}

/// Running series whose generated occurrences end before `horizon` (a local date).
pub async fn pending_series<R: Runtime>(rt: &R, horizon: &str, limit: i64) -> Result<Vec<Series>> {
    let rows = rt
        .query(
            &format!(
                "{SERIES_SELECT} WHERE s.stopped_at IS NULL \
                 AND (s.generated_until IS NULL OR s.generated_until < ?) LIMIT {}",
                limit.clamp(1, 50)
            ),
            vec![horizon.into()],
        )
        .await?;
    Ok(rows.iter().map(Series::from_row).collect())
}

/// Local dates from `from` on that already have an issue (kept or picked up earlier).
pub async fn existing_occurrences<R: Runtime>(
    rt: &R,
    series_id: &str,
    from: &str,
) -> Result<BTreeSet<String>> {
    let rows = rt
        .query(
            "SELECT occurrence FROM issues WHERE series_id = ? AND occurrence >= ?",
            vec![series_id.into(), from.into()],
        )
        .await?;
    Ok(rows.iter().map(|r| r.text("occurrence")).collect())
}

/// Creates one issue per `(date, start)` with the series' labels and members' reminders,
/// and records that dates up to `generated_until` are done. A concurrent run makes the
/// batch fail on the occurrence index instead of creating duplicates.
pub async fn generate_occurrences<R: Runtime>(
    rt: &R,
    series: &Series,
    occurrences: &[(NaiveDate, i64)],
    generated_until: NaiveDate,
) -> Result<()> {
    let now = rt.now_ms();
    let assignee = series
        .assignee_id
        .clone()
        .filter(|_| series.assignee_active);
    let search = search_text(&series.title, &series.body);
    let duration = series.rule.duration_min * 60_000;
    let ids: Vec<String> = occurrences.iter().map(|_| new_id(rt)).collect();
    let mut stmts = Vec::new();
    let rows_per_stmt = MAX_IN_LIST / OCCURRENCE_COLUMNS;
    for (chunk_ids, chunk) in ids
        .chunks(rows_per_stmt)
        .zip(occurrences.chunks(rows_per_stmt))
    {
        let mut params = Vec::with_capacity(chunk.len() * OCCURRENCE_COLUMNS);
        for (id, (date, start)) in chunk_ids.iter().zip(chunk) {
            params.extend([
                id.as_str().into(),
                series.title.as_str().into(),
                series.body.as_str().into(),
                search.as_str().into(),
                Status::Open.as_str().into(),
                series.visibility.as_str().into(),
                series.issuer_id.as_str().into(),
                assignee.clone().into(),
                (*start).into(),
                (start + duration).into(),
                now.into(),
                now.into(),
                series.id.as_str().into(),
                format_date(*date).into(),
            ]);
        }
        let row = format!("({})", ["?"; OCCURRENCE_COLUMNS].join(", "));
        stmts.push(Stmt::new(
            format!(
                "INSERT INTO issues (id, title, body, search_text, status, visibility, issuer_id, \
                 assignee_id, start_at, due_at, created_at, updated_at, series_id, occurrence) VALUES {}",
                vec![row; chunk.len()].join(", ")
            ),
            params,
        ));
    }
    let fire = occurrence_fire_sql("sr.mode", "sr.offset_min");
    for chunk in ids.chunks(MAX_IN_LIST) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let chunk_params = || chunk.iter().map(|id| id.as_str().into());
        stmts.push(Stmt::new(
            format!(
                "INSERT INTO issue_labels (issue_id, label_id) SELECT i.id, sl.label_id \
                 FROM issues i JOIN series_labels sl ON sl.series_id = i.series_id \
                 WHERE i.id IN ({placeholders})"
            ),
            chunk_params().collect(),
        ));
        let mut params: Vec<Value> = chunk_params().collect();
        params.push(now.into());
        stmts.push(Stmt::new(
            format!(
                "INSERT INTO reminders (issue_id, user_id, mode, offset_min, remind_at, notified_at) \
                 SELECT i.id, sr.user_id, sr.mode, sr.offset_min, {fire}, NULL \
                 FROM issues i JOIN series_reminders sr ON sr.series_id = i.series_id \
                 WHERE i.id IN ({placeholders}) AND {fire} > ?"
            ),
            params,
        ));
    }
    // An edit meanwhile resets the progress; do not overwrite that.
    stmts.push(Stmt::new(
        "UPDATE series SET generated_until = ? WHERE id = ? AND updated_at = ?",
        vec![
            format_date(generated_until).into(),
            series.id.as_str().into(),
            series.updated_at.into(),
        ],
    ));
    rt.batch(stmts).await
}

// ---------------------------------------------------------------- reminders

/// Moves `mode`'s relative reminders to a changed anchor time, or drops them when it is cleared.
fn follow_anchor(issue_id: &str, mode: &'static str, anchor: Option<i64>) -> Stmt {
    match anchor {
        Some(at) => Stmt::new(
            "UPDATE reminders SET remind_at = ? - offset_min * 60000, notified_at = NULL \
             WHERE issue_id = ? AND mode = ? AND remind_at <> ? - offset_min * 60000",
            vec![at.into(), issue_id.into(), mode.into(), at.into()],
        ),
        None => Stmt::new(
            "DELETE FROM reminders WHERE issue_id = ? AND mode = ?",
            vec![issue_id.into(), mode.into()],
        ),
    }
}

fn reminder_stmt(
    issue_id: &str,
    user_id: &str,
    reminder: Reminder,
    start_at: Option<i64>,
    due_at: Option<i64>,
) -> Option<Stmt> {
    let fire_at = reminder.fire_at(start_at, due_at)?;
    let (mode, offset) = reminder.mode();
    Some(Stmt::new(
        "INSERT INTO reminders (issue_id, user_id, mode, offset_min, remind_at, notified_at) \
         VALUES (?, ?, ?, ?, ?, NULL)",
        vec![
            issue_id.into(),
            user_id.into(),
            mode.into(),
            offset.into(),
            fire_at.into(),
        ],
    ))
}

pub async fn find_reminder<R: Runtime>(
    rt: &R,
    issue_id: &str,
    user_id: &str,
) -> Result<Option<(Reminder, i64, bool)>> {
    let rows = rt
        .query(
            "SELECT mode, offset_min, remind_at, notified_at FROM reminders WHERE issue_id = ? AND user_id = ?",
            vec![issue_id.into(), user_id.into()],
        )
        .await?;
    Ok(rows.first().and_then(|row| {
        Reminder::from_row(row).map(|r| {
            (
                r,
                row.int("remind_at"),
                row.opt_int("notified_at").is_some(),
            )
        })
    }))
}

/// Replaces the user's reminder; `None` removes it.
pub async fn set_reminder<R: Runtime>(
    rt: &R,
    issue_id: &str,
    user_id: &str,
    reminder: Option<Reminder>,
    start_at: Option<i64>,
    due_at: Option<i64>,
) -> Result<()> {
    let mut stmts = vec![Stmt::new(
        "DELETE FROM reminders WHERE issue_id = ? AND user_id = ?",
        vec![issue_id.into(), user_id.into()],
    )];
    if let Some(stmt) = reminder.and_then(|r| reminder_stmt(issue_id, user_id, r, start_at, due_at))
    {
        stmts.push(stmt);
    }
    rt.batch(stmts).await
}

pub struct DueReminder {
    pub issue_id: String,
    pub title: String,
    pub remind_at: i64,
    pub due_at: Option<i64>,
}

/// Reminders that should fire now and were not shown yet, for issues the user can still see.
pub async fn due_reminders<R: Runtime>(rt: &R, user_id: &str) -> Result<Vec<DueReminder>> {
    let rows = rt
        .query(
            &format!(
                "SELECT r.issue_id, r.remind_at, i.title, i.due_at FROM reminders r \
                 JOIN issues i ON i.id = r.issue_id \
                 WHERE r.user_id = ? AND r.notified_at IS NULL AND r.remind_at <= ? \
                 AND i.status <> 'done' AND {VISIBLE_TO} ORDER BY r.remind_at LIMIT 20"
            ),
            vec![
                user_id.into(),
                rt.now_ms().into(),
                user_id.into(),
                user_id.into(),
            ],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| DueReminder {
            issue_id: r.text("issue_id"),
            title: r.text("title"),
            remind_at: r.int("remind_at"),
            due_at: r.opt_int("due_at"),
        })
        .collect())
}

/// When the user's next pending reminder fires, so the page can poll right then.
pub async fn next_reminder_at<R: Runtime>(rt: &R, user_id: &str) -> Result<Option<i64>> {
    let rows = rt
        .query(
            &format!(
                "SELECT MIN(r.remind_at) AS next_at FROM reminders r \
                 JOIN issues i ON i.id = r.issue_id \
                 WHERE r.user_id = ? AND r.notified_at IS NULL AND r.remind_at > ? \
                 AND i.status <> 'done' AND {VISIBLE_TO}"
            ),
            vec![
                user_id.into(),
                rt.now_ms().into(),
                user_id.into(),
                user_id.into(),
            ],
        )
        .await?;
    Ok(rows.first().and_then(|r| r.opt_int("next_at")))
}

/// Returns false when another tab already acknowledged it, so only one shows it.
pub async fn ack_reminder<R: Runtime>(rt: &R, issue_id: &str, user_id: &str) -> Result<bool> {
    let changed = rt
        .execute(
            "UPDATE reminders SET notified_at = ? WHERE issue_id = ? AND user_id = ? AND notified_at IS NULL",
            vec![rt.now_ms().into(), issue_id.into(), user_id.into()],
        )
        .await?;
    Ok(changed > 0)
}

pub async fn upcoming_reminders<R: Runtime>(
    rt: &R,
    user_id: &str,
    limit: i64,
) -> Result<Vec<DueReminder>> {
    let rows = rt
        .query(
            &format!(
                "SELECT r.issue_id, r.remind_at, i.title, i.due_at FROM reminders r \
                 JOIN issues i ON i.id = r.issue_id \
                 WHERE r.user_id = ? AND r.notified_at IS NULL AND i.status <> 'done' AND {VISIBLE_TO} \
                 ORDER BY r.remind_at LIMIT {}",
                limit.clamp(1, 50)
            ),
            vec![user_id.into(), user_id.into(), user_id.into()],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| DueReminder {
            issue_id: r.text("issue_id"),
            title: r.text("title"),
            remind_at: r.int("remind_at"),
            due_at: r.opt_int("due_at"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Row;
    use std::sync::{Arc, Mutex};

    /// Records batches so tests can check which value lands in which column.
    #[derive(Clone, Default)]
    struct Recorder {
        batches: Arc<Mutex<Vec<Vec<Stmt>>>>,
        /// Returned by every query.
        rows: Vec<Row>,
    }

    impl Runtime for Recorder {
        async fn query(&self, _: &str, _: Vec<Value>) -> Result<Vec<Row>> {
            Ok(self.rows.clone())
        }
        async fn execute(&self, _: &str, _: Vec<Value>) -> Result<u64> {
            Ok(1)
        }
        async fn batch(&self, stmts: Vec<Stmt>) -> Result<()> {
            self.batches.lock().unwrap().push(stmts);
            Ok(())
        }
        fn now_ms(&self) -> i64 {
            1_000
        }
        fn fill_random(&self, buf: &mut [u8]) {
            buf.fill(1);
        }
        fn log_error(&self, _: &str) {}
    }

    /// Pairs the column list of an `INSERT INTO t (a, b) VALUES (?, ?)` with its params.
    fn columns(stmt: &Stmt) -> HashMap<String, Value> {
        let list = &stmt.sql[stmt.sql.find('(').unwrap() + 1..stmt.sql.find(')').unwrap()];
        list.split(',')
            .map(|c| c.trim().to_owned())
            .zip(stmt.params.iter().cloned())
            .collect()
    }

    #[tokio::test]
    async fn completing_setup_signs_out_other_sessions() {
        let rt = Recorder {
            // The read-back after the batch finds this request's hash.
            rows: vec![Row::new(HashMap::from([(
                "password_hash".to_owned(),
                Value::from("hash"),
            )]))],
            ..Recorder::default()
        };
        let site = SiteUpdate {
            site_name: "Acme".into(),
            site_url: String::new(),
            timezone: "UTC".into(),
            allow_signup: false,
        };
        let admin = AdminSetup {
            admin_id: "admin-id".into(),
            display_name: "Boss".into(),
            email: "boss@example.com".into(),
            password_hash: "hash".into(),
            keep_session: "mine".into(),
        };
        assert!(complete_setup(&rt, &site, admin).await.unwrap());
        let batches = rt.batches.lock().unwrap();
        let logout = batches[0]
            .iter()
            .find(|s| s.sql.starts_with("DELETE FROM sessions"))
            .expect("other sessions are ended");
        assert_eq!(
            logout.params,
            vec![
                Value::from("admin-id"),
                Value::from(token_key("mine")),
                Value::from("admin-id")
            ]
        );
        let claim = batches[0].last().unwrap();
        assert!(claim.sql.contains("setup_completed"), "the flag flips last");
        assert!(
            batches[0]
                .iter()
                .any(|s| s.params.contains(&Value::from("Acme")))
        );
    }

    #[tokio::test]
    async fn create_issue_binds_each_value_to_its_column() {
        let rt = Recorder::default();
        let input = IssueInput {
            title: "Fix ÉCOLE login".into(),
            body: "Body".into(),
            status: Status::Open,
            visibility: Visibility::Public,
            assignee_id: None,
            start_at: Some(5),
            due_at: Some(9),
            label_ids: Vec::new(),
        };
        create_issue(&rt, "issuer", input, None).await.unwrap();
        let batches = rt.batches.lock().unwrap();
        let insert = columns(&batches[0][0]);
        assert_eq!(insert["title"], Value::from("Fix ÉCOLE login"));
        assert_eq!(insert["body"], Value::from("Body"));
        assert_eq!(insert["search_text"], Value::from("fix école login\nbody"));
        assert_eq!(insert["issuer_id"], Value::from("issuer"));
        assert_eq!(insert["status"], Value::from("open"));
        assert_eq!(insert["visibility"], Value::from("public"));
        assert_eq!(insert["start_at"], Value::Int(5));
        assert_eq!(insert["due_at"], Value::Int(9));
        assert_eq!(insert["assignee_id"], Value::Null);
    }

    #[tokio::test]
    async fn generated_occurrences_bind_each_row_to_its_columns() {
        use crate::recurrence::{Freq, parse_date};
        let rt = Recorder::default();
        let day = parse_date("2026-10-28").unwrap();
        let series = Series {
            id: "series".into(),
            title: "週報".into(),
            body: String::new(),
            visibility: Visibility::Public,
            issuer_id: "issuer".into(),
            issuer_name: String::new(),
            issuer_active: true,
            assignee_id: Some("gone".into()),
            assignee_name: None,
            assignee_active: false,
            rule: Recurrence {
                freq: Freq::Daily,
                weekdays: 0,
                start_date: day,
                until: None,
                start_minute: 0,
                duration_min: 60,
            },
            timezone: chrono_tz::Asia::Taipei,
            effective_from: 0,
            generated_until: None,
            stopped_at: None,
            created_at: 0,
            updated_at: 42,
            labels: Vec::new(),
        };
        // Seven rows need two INSERTs under D1's parameter limit.
        let occurrences: Vec<_> = day
            .iter_days()
            .take(7)
            .enumerate()
            .map(|(i, d)| (d, 1_000 * i as i64))
            .collect();
        generate_occurrences(&rt, &series, &occurrences, day)
            .await
            .unwrap();
        let batches = rt.batches.lock().unwrap();
        let inserts: Vec<&Stmt> = batches[0]
            .iter()
            .filter(|s| s.sql.starts_with("INSERT INTO issues"))
            .collect();
        assert_eq!(inserts.len(), 2);
        assert!(inserts.iter().all(|s| s.params.len() <= 100));
        let last = &inserts[1].params[..OCCURRENCE_COLUMNS];
        let row = columns(&Stmt::new(inserts[1].sql.clone(), last.to_vec()));
        assert_eq!(row["title"], Value::from("週報"));
        assert_eq!(
            row["assignee_id"],
            Value::Null,
            "inactive assignee is dropped"
        );
        assert_eq!(row["start_at"], Value::Int(6_000));
        assert_eq!(row["due_at"], Value::Int(6_000 + 3_600_000));
        assert_eq!(row["series_id"], Value::from("series"));
        assert_eq!(row["occurrence"], Value::from("2026-11-03"));
        let progress = batches[0].last().unwrap();
        assert!(
            progress
                .sql
                .starts_with("UPDATE series SET generated_until")
        );
        assert_eq!(
            progress.params[2],
            Value::Int(42),
            "guarded by the version read"
        );
    }
}
