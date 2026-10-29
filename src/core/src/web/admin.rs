//! User management and system settings; administrators only.

use axum::extract::{Form, Path, State};
use axum::http::HeaderMap;

use super::forms::{self, FormData};
use super::site::{HTTPS_UNREACHABLE, https_unreachable, timezone_options};
use super::views::{AdminPage, AdminUserRow, SiteForm};
use super::{App, AppError, Ctx, FlashKind, HandlerResult, redirect_flash, render};
use crate::db::{DbError, Runtime};
use crate::domain::User;
use crate::ids::is_valid_id;
use crate::{password, store};

fn require_admin(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.user.is_admin {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

/// Target user for a per-user action; administrators cannot demote or disable themselves.
async fn target_user<R: Runtime>(app: &App<R>, ctx: &Ctx, id: &str) -> Result<User, AppError> {
    require_admin(ctx)?;
    if !is_valid_id(id) {
        return Err(AppError::NotFound);
    }
    let user = store::find_user(&app.rt, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if user.id == ctx.user.id {
        return Err(AppError::Forbidden);
    }
    Ok(user)
}

pub async fn page<R: Runtime>(State(app): State<App<R>>, ctx: Ctx) -> HandlerResult {
    require_admin(&ctx)?;
    let users = store::list_users(&app.rt, false)
        .await?
        .into_iter()
        .map(|u| AdminUserRow {
            is_self: u.id == ctx.user.id,
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            email: u.email.unwrap_or_default(),
            is_admin: u.is_admin,
            is_active: u.is_active,
            must_change_password: u.must_change_password,
        })
        .collect();
    render(
        &AdminPage {
            layout: ctx.layout("管理後台", "admin"),
            users,
            site: SiteForm::from_site(&ctx.site),
            timezones: timezone_options(ctx.site.tz.name()),
        },
        ctx.had_flash(),
    )
}

fn temp_password_hash<R: Runtime>(app: &App<R>, form: &FormData) -> Result<String, &'static str> {
    let temp = form.get("password");
    password::check_policy(temp)?;
    let mut salt = [0u8; 16];
    app.rt.fill_random(&mut salt);
    Ok(password::hash(temp, &salt))
}

pub async fn create_user<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    require_admin(&ctx)?;
    let form = FormData(raw);
    let parsed = forms::normalize_username(form.get("username")).and_then(|username| {
        Ok((
            username,
            forms::display_name(form.get("display_name"))?,
            forms::optional_email(form.get("email"))?,
            temp_password_hash(&app, &form)?,
        ))
    });
    let (username, display_name, email, password_hash) = match parsed {
        Ok(values) => values,
        Err(e) => return Ok(redirect_flash("/admin", FlashKind::Err, e)),
    };
    let created = store::create_user(
        &app.rt,
        store::NewUser {
            username: &username,
            display_name: &display_name,
            email: email.as_deref(),
            password_hash,
            is_admin: form.checked("is_admin"),
            must_change_password: true,
        },
    )
    .await;
    match created {
        Ok(_) => Ok(redirect_flash(
            "/admin",
            FlashKind::Ok,
            &format!("已新增使用者「{username}」，首次登入需修改密碼"),
        )),
        Err(DbError::UniqueViolation) => {
            Ok(redirect_flash("/admin", FlashKind::Err, "這個帳號已經存在"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn reset_password<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let user = target_user(&app, &ctx, &id).await?;
    match temp_password_hash(&app, &FormData(raw)) {
        Ok(hash) => {
            store::set_password(&app.rt, &user.id, hash, true, None).await?;
            Ok(redirect_flash(
                "/admin",
                FlashKind::Ok,
                &format!("已重設「{}」的密碼，下次登入需修改", user.username),
            ))
        }
        Err(e) => Ok(redirect_flash("/admin", FlashKind::Err, e)),
    }
}

/// The buttons send the state they show, so a stale page cannot flip it back.
fn wanted_flag(form: &FormData) -> Result<bool, AppError> {
    match form.get("value") {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(AppError::NotFound),
    }
}

pub async fn toggle_admin<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let user = target_user(&app, &ctx, &id).await?;
    let make_admin = wanted_flag(&FormData(raw))?;
    store::set_admin(&app.rt, &user.id, make_admin).await?;
    let message = if make_admin {
        "已設為管理員"
    } else {
        "已取消管理員"
    };
    Ok(redirect_flash(
        "/admin",
        FlashKind::Ok,
        &format!("「{}」{message}", user.username),
    ))
}

pub async fn toggle_active<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Path(id): Path<String>,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let user = target_user(&app, &ctx, &id).await?;
    let activate = wanted_flag(&FormData(raw))?;
    store::set_active(&app.rt, &user.id, activate).await?;
    let message = if activate {
        "已重新啟用"
    } else {
        "已停用並登出，未完成的工作已改為未指派"
    };
    Ok(redirect_flash(
        "/admin",
        FlashKind::Ok,
        &format!("「{}」{message}", user.username),
    ))
}

pub async fn settings<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    headers: HeaderMap,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    require_admin(&ctx)?;
    match SiteForm::from_post(&FormData(raw)).validate() {
        Ok(update) if https_unreachable(&update.site_url, &headers) => {
            Ok(redirect_flash("/admin", FlashKind::Err, HTTPS_UNREACHABLE))
        }
        Ok(update) => {
            store::save_settings(&app.rt, &update).await?;
            app.forget_site();
            Ok(redirect_flash("/admin", FlashKind::Ok, "網站設定已儲存"))
        }
        Err(errors) => Ok(redirect_flash("/admin", FlashKind::Err, &errors.join("；"))),
    }
}
