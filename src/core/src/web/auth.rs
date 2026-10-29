//! Sessions, login/logout, self sign-up and password changes.

use axum::extract::{Form, FromRequestParts, Query, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;

use super::forms::{self, FormData};
use super::site::Site;
use super::views::{Layout, LoginPage, PasswordPage, SignupPage};
use super::{
    App, AppError, Flash, FlashKind, HandlerResult, cookie, read_flash, redirect, redirect_flash,
    render, safe_next, session_cookie, with_cookie,
};
use crate::db::Runtime;
use crate::domain::User;
use crate::ids::new_token;
use crate::{password, store};

const SESSION_COOKIE: &str = "sid";

/// A logged-in user who has completed any required password change.
pub struct Ctx {
    pub user: User,
    pub token: String,
    pub flash: Option<Flash>,
    pub site: Site,
    /// Server time when the request was loaded (Unix milliseconds).
    pub now: i64,
}

/// A logged-in user who may still have to change the password.
pub struct PwCtx(pub Ctx);

impl Ctx {
    pub fn layout(&self, title: &str, nav: &'static str) -> Layout {
        Layout {
            asset_version: super::ASSET_VERSION,
            brand: self.site.name.clone(),
            title: title.to_owned(),
            nav,
            signed_in: true,
            user_name: self.user.display_name.clone(),
            is_admin: self.user.is_admin,
            restricted: self.user.must_change_password,
            now_ms: self.now,
            flash: self.flash.as_ref().map(|f| Flash {
                kind: f.kind,
                text: f.text.clone(),
            }),
        }
    }

    pub fn had_flash(&self) -> bool {
        self.flash.is_some()
    }
}

async fn load<R: Runtime>(parts: &Parts, app: &App<R>) -> Result<Option<Ctx>, AppError> {
    let Some(token) = cookie(&parts.headers, SESSION_COOKIE) else {
        return Ok(None);
    };
    let Some(user) = store::session_user(&app.rt, &token).await? else {
        return Ok(None);
    };
    Ok(Some(Ctx {
        user,
        token,
        flash: read_flash(&parts.headers),
        site: app.site().await?,
        now: app.rt.now_ms(),
    }))
}

fn login_redirect(parts: &Parts) -> Response {
    if parts.uri.path().starts_with("/api/") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let here = if parts.method != axum::http::Method::GET {
        // Form actions are POST-only; after login go back to the page the form was on.
        post_return_path(parts.uri.path())
    } else {
        match parts.uri.query() {
            Some(q) => format!("{}?{q}", parts.uri.path()),
            None => parts.uri.path().to_owned(),
        }
    };
    let next: String = form_urlencoded::byte_serialize(here.as_bytes()).collect();
    redirect(&format!("/login?next={next}"))
}

/// Pages send the user where they must go next; the JSON API just refuses.
fn gate(parts: &Parts, page: &str) -> Response {
    if parts.uri.path().starts_with("/api/") {
        StatusCode::FORBIDDEN.into_response()
    } else {
        redirect(page)
    }
}

/// `/issues/{id}/claim` → `/issues/{id}`; other actions → the dashboard.
fn post_return_path(path: &str) -> String {
    match path
        .strip_prefix("/issues/")
        .and_then(|rest| rest.split('/').next())
    {
        Some(id) if crate::ids::is_valid_id(id) => format!("/issues/{id}"),
        _ => "/".to_owned(),
    }
}

impl<R: Runtime> FromRequestParts<App<R>> for Ctx {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &App<R>) -> Result<Self, Self::Rejection> {
        match load(parts, app).await {
            // First-run setup comes before everything, including a password change.
            Ok(Some(ctx)) if !ctx.site.setup_completed => Err(gate(parts, "/setup")),
            Ok(Some(ctx)) if ctx.user.must_change_password => Err(gate(parts, "/password")),
            Ok(Some(ctx)) => {
                app.generate_series(ctx.site.tz).await;
                Ok(ctx)
            }
            Ok(None) => Err(login_redirect(parts)),
            Err(e) => Err(e.into_response()),
        }
    }
}

impl<R: Runtime> FromRequestParts<App<R>> for PwCtx {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &App<R>) -> Result<Self, Self::Rejection> {
        match load(parts, app).await {
            Ok(Some(ctx)) => Ok(PwCtx(ctx)),
            Ok(None) => Err(login_redirect(parts)),
            Err(e) => Err(e.into_response()),
        }
    }
}

pub(super) fn anonymous_layout(title: &str, headers: &HeaderMap, site: &Site) -> Layout {
    Layout {
        asset_version: super::ASSET_VERSION,
        brand: site.name.clone(),
        title: title.to_owned(),
        nav: "",
        signed_in: false,
        user_name: String::new(),
        is_admin: false,
        restricted: false,
        now_ms: 0,
        flash: read_flash(headers),
    }
}

async fn start_session<R: Runtime>(app: &App<R>, user: &User, next: &str) -> HandlerResult {
    let site = app.site().await?;
    let token = new_token(&app.rt);
    store::create_session(&app.rt, &token, &user.id).await?;
    let target = if !site.setup_completed {
        "/setup".to_owned()
    } else if user.must_change_password {
        "/password".to_owned()
    } else {
        safe_next(next, "/")
    };
    Ok(with_cookie(
        redirect(&target),
        session_cookie(&token, site.cookie_secure),
    ))
}

// ---------------------------------------------------------------- login

pub async fn login_page<R: Runtime>(
    State(app): State<App<R>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> HandlerResult {
    if let Some(token) = cookie(&headers, SESSION_COOKIE)
        && store::session_user(&app.rt, &token).await?.is_some()
    {
        return Ok(redirect("/"));
    }
    let site = app.site().await?;
    let layout = anonymous_layout("登入", &headers, &site);
    let had_flash = layout.flash.is_some();
    render(
        &LoginPage {
            layout,
            setup_pending: !site.setup_completed,
            allow_signup: site.allow_signup,
            error: String::new(),
            username: String::new(),
            next: query.get("next").cloned().unwrap_or_default(),
        },
        had_flash,
    )
}

pub async fn login<R: Runtime>(
    State(app): State<App<R>>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(form);
    let username = form.get("username").trim().to_lowercase();
    let found = store::find_login(&app.rt, &username).await?;
    let now = app.rt.now_ms();
    let error = match &found {
        Some(record) if record.locked_until.is_some_and(|until| until > now) => {
            "登入失敗次數過多，帳號暫時鎖定，請 10 分鐘後再試"
        }
        // Verify before checking is_active so disabled accounts take as long as the rest.
        Some(record) if password::verify(form.get("password"), &record.password_hash) => {
            if record.user.is_active {
                store::clear_failed_logins(&app.rt, &record.user.id).await?;
                return start_session(&app, &record.user, form.get("next")).await;
            }
            "帳號或密碼錯誤，或帳號已停用"
        }
        Some(record) => {
            store::record_failed_login(&app.rt, &record.user.id).await?;
            "帳號或密碼錯誤，或帳號已停用"
        }
        None => {
            // Spend the same hashing time so response timing does not reveal unknown accounts.
            password::verify(form.get("password"), password::DUMMY_HASH);
            "帳號或密碼錯誤，或帳號已停用"
        }
    };
    let site = app.site().await?;
    let page = LoginPage {
        layout: anonymous_layout("登入", &headers, &site),
        setup_pending: !site.setup_completed,
        allow_signup: site.allow_signup,
        error: error.to_owned(),
        username,
        next: form.get("next").to_owned(),
    };
    render(&page, false).map(|r| (StatusCode::UNAUTHORIZED, r).into_response())
}

pub async fn logout<R: Runtime>(State(app): State<App<R>>, headers: HeaderMap) -> HandlerResult {
    if let Some(token) = cookie(&headers, SESSION_COOKIE) {
        store::delete_session(&app.rt, &token).await?;
    }
    Ok(with_cookie(
        redirect_flash("/login", FlashKind::Ok, "已登出"),
        format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax"),
    ))
}

// ---------------------------------------------------------------- sign-up

pub async fn signup_page<R: Runtime>(
    State(app): State<App<R>>,
    headers: HeaderMap,
) -> HandlerResult {
    let site = app.site().await?;
    if !site.allow_signup {
        return Err(AppError::NotFound);
    }
    render(
        &SignupPage {
            layout: anonymous_layout("註冊帳號", &headers, &site),
            error: String::new(),
            username: String::new(),
            display_name: String::new(),
        },
        false,
    )
}

pub async fn signup<R: Runtime>(
    State(app): State<App<R>>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let site = app.site().await?;
    if !site.allow_signup {
        return Err(AppError::NotFound);
    }
    let form = FormData(form);
    let result = validate_signup(&form);
    let (username, display_name, hash_input) = match result {
        Ok(values) => values,
        Err(error) => {
            return render(
                &SignupPage {
                    layout: anonymous_layout("註冊帳號", &headers, &site),
                    error: error.to_owned(),
                    username: form.get("username").to_owned(),
                    display_name: form.get("display_name").to_owned(),
                },
                false,
            );
        }
    };
    let mut salt = [0u8; 16];
    app.rt.fill_random(&mut salt);
    let created = store::create_user(
        &app.rt,
        store::NewUser {
            username: &username,
            display_name: &display_name,
            email: None,
            password_hash: password::hash(&hash_input, &salt),
            is_admin: false,
            must_change_password: false,
        },
    )
    .await;
    match created {
        Ok(_) => {
            let user = store::find_login(&app.rt, &username)
                .await?
                .map(|record| record.user)
                .ok_or(AppError::NotFound)?;
            start_session(&app, &user, "/").await
        }
        Err(crate::db::DbError::UniqueViolation) => render(
            &SignupPage {
                layout: anonymous_layout("註冊帳號", &headers, &site),
                error: "這個帳號已經有人使用".to_owned(),
                username,
                display_name,
            },
            false,
        ),
        Err(e) => Err(e.into()),
    }
}

fn validate_signup(form: &FormData) -> Result<(String, String, String), &'static str> {
    let username = forms::normalize_username(form.get("username"))?;
    let display_name = forms::display_name(form.get("display_name"))?;
    let new_password = form.get("password");
    password::check_policy(new_password)?;
    if new_password != form.get("password_confirm") {
        return Err("兩次輸入的密碼不一致");
    }
    Ok((username, display_name, new_password.to_owned()))
}

// ---------------------------------------------------------------- password

// `State` ties the generic runtime to the extractor; the page itself needs no data.
pub async fn password_page<R: Runtime>(_: State<App<R>>, PwCtx(ctx): PwCtx) -> HandlerResult {
    password_view(&ctx, String::new())
}

fn password_view(ctx: &Ctx, error: String) -> HandlerResult {
    render(
        &PasswordPage {
            layout: ctx.layout("帳號設定", "account"),
            forced: ctx.user.must_change_password,
            error,
            username: ctx.user.username.clone(),
            display_name: ctx.user.display_name.clone(),
            email: ctx.user.email.clone().unwrap_or_default(),
        },
        ctx.had_flash(),
    )
}

pub async fn change_password<R: Runtime>(
    State(app): State<App<R>>,
    PwCtx(ctx): PwCtx,
    Form(form): Form<Vec<(String, String)>>,
) -> HandlerResult {
    // Before setup the seeded password may only be replaced through /setup, which also
    // ends the other sessions; otherwise an intruder could lock the operator out here.
    if !ctx.site.setup_completed {
        return Ok(redirect("/setup"));
    }
    let form = FormData(form);
    let current = form.get("current_password");
    let new_password = form.get("new_password");
    let stored = store::password_hash(&app.rt, &ctx.user.id).await?;
    let error = if !password::verify(current, &stored) {
        Some("目前密碼不正確")
    } else if new_password != form.get("new_password_confirm") {
        Some("兩次輸入的新密碼不一致")
    } else if new_password == current {
        Some("新密碼不可與目前密碼相同")
    } else {
        password::check_policy(new_password).err()
    };
    if let Some(error) = error {
        return password_view(&ctx, error.to_owned());
    }
    let mut salt = [0u8; 16];
    app.rt.fill_random(&mut salt);
    store::set_password(
        &app.rt,
        &ctx.user.id,
        password::hash(new_password, &salt),
        false,
        Some(&ctx.token),
    )
    .await?;
    Ok(redirect_flash("/", FlashKind::Ok, "密碼已更新"))
}

pub async fn change_profile<R: Runtime>(
    State(app): State<App<R>>,
    ctx: Ctx,
    Form(form): Form<Vec<(String, String)>>,
) -> HandlerResult {
    let form = FormData(form);
    let parsed = forms::display_name(form.get("display_name"))
        .and_then(|name| Ok((name, forms::optional_email(form.get("email"))?)));
    match parsed {
        Ok((name, email)) => {
            store::set_profile(&app.rt, &ctx.user.id, &name, email.as_deref()).await?;
            Ok(redirect_flash("/password", FlashKind::Ok, "個人資料已更新"))
        }
        Err(e) => Ok(redirect_flash("/password", FlashKind::Err, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_actions_return_to_their_page() {
        assert_eq!(
            post_return_path("/issues/01ARZ3NDEKTSV4RRFFQ69G5FAV/claim"),
            "/issues/01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        assert_eq!(post_return_path("/admin/users/x/active"), "/");
        assert_eq!(post_return_path("/issues/not-an-id/claim"), "/");
    }
}
