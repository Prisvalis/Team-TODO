//! First-run setup: the seeded administrator names the site and sets a real password.

use axum::extract::{Form, State};
use axum::http::HeaderMap;

use super::forms::{self, FormData};
use super::site::{HTTPS_UNREACHABLE, https_unreachable, timezone_options};
use super::views::{SetupPage, SiteForm};
use super::{App, Ctx, FlashKind, HandlerResult, PwCtx, redirect, redirect_flash, render};
use crate::db::Runtime;
use crate::{password, store};

/// What the page shows besides the site form.
struct SetupInput {
    site: SiteForm,
    display_name: String,
    email: String,
    errors: Vec<String>,
}

/// Setup is for an administrator, and only once. Other users signed in before setup
/// see a waiting notice; `Some` is the response to send instead of the form.
fn blocked(ctx: &Ctx) -> Option<HandlerResult> {
    if ctx.site.setup_completed {
        return Some(Ok(redirect("/")));
    }
    if !ctx.user.is_admin {
        return Some(view(ctx, false, None));
    }
    None
}

/// Renders the waiting notice (`form: None`) or the administrator's form.
fn view(ctx: &Ctx, needs_token: bool, form: Option<SetupInput>) -> HandlerResult {
    let mut layout = ctx.layout("初始設定", "");
    layout.restricted = true;
    let waiting = form.is_none();
    let form = form.unwrap_or_else(|| SetupInput {
        site: SiteForm::from_site(&ctx.site),
        display_name: String::new(),
        email: String::new(),
        errors: Vec::new(),
    });
    render(
        &SetupPage {
            timezones: timezone_options(&form.site.timezone),
            layout,
            errors: form.errors,
            site: form.site,
            display_name: form.display_name,
            email: form.email,
            waiting,
            needs_token,
        },
        ctx.had_flash(),
    )
}

/// Constant-time so response timing does not leak how much of the token matched.
fn token_matches(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

pub async fn page<R: Runtime>(State(app): State<App<R>>, PwCtx(ctx): PwCtx) -> HandlerResult {
    if let Some(response) = blocked(&ctx) {
        return response;
    }
    let form = SetupInput {
        site: SiteForm::from_site(&ctx.site),
        display_name: ctx.user.display_name.clone(),
        email: ctx.user.email.clone().unwrap_or_default(),
        errors: Vec::new(),
    };
    view(&ctx, app.cfg.setup_token.is_some(), Some(form))
}

pub async fn submit<R: Runtime>(
    State(app): State<App<R>>,
    PwCtx(ctx): PwCtx,
    headers: HeaderMap,
    Form(raw): Form<Vec<(String, String)>>,
) -> HandlerResult {
    if let Some(response) = blocked(&ctx) {
        return response;
    }
    let form = FormData(raw);
    let site_form = SiteForm::from_post(&form);
    let mut errors = Vec::new();
    if let Some(expected) = &app.cfg.setup_token
        && !token_matches(expected, form.get("setup_token").trim())
    {
        errors.push("設定金鑰不正確（見部署設定的 SETUP_TOKEN）".to_owned());
    }
    let site = site_form.validate().map_err(|e| errors.extend(e));
    if let Ok(update) = &site
        && https_unreachable(&update.site_url, &headers)
    {
        errors.push(HTTPS_UNREACHABLE.to_owned());
    }
    let display_name =
        forms::display_name(form.get("display_name")).map_err(|e| errors.push(e.to_owned()));
    let email = forms::email(form.get("email")).map_err(|e| errors.push(e.to_owned()));
    let new_password = form.get("new_password");
    let stored_hash = store::password_hash(&app.rt, &ctx.user.id).await?;
    if let Err(e) = password::check_policy(new_password) {
        errors.push(e.to_owned());
    } else if new_password != form.get("new_password_confirm") {
        errors.push("兩次輸入的新密碼不一致".to_owned());
    } else if password::verify(new_password, &stored_hash) {
        errors.push("新密碼不可與預設密碼相同".to_owned());
    }
    let (Ok(site), Ok(display_name), Ok(email), true) =
        (site, display_name, email, errors.is_empty())
    else {
        let again = SetupInput {
            site: site_form,
            display_name: form.get("display_name").to_owned(),
            email: form.get("email").to_owned(),
            errors,
        };
        return view(&ctx, app.cfg.setup_token.is_some(), Some(again));
    };
    let mut salt = [0u8; 16];
    app.rt.fill_random(&mut salt);
    let finished = store::complete_setup(
        &app.rt,
        &site,
        store::AdminSetup {
            admin_id: ctx.user.id.clone(),
            display_name,
            email,
            password_hash: password::hash(new_password, &salt),
            keep_session: ctx.token.clone(),
        },
    )
    .await?;
    app.forget_site();
    if finished {
        Ok(redirect_flash("/", FlashKind::Ok, "初始設定完成，歡迎使用"))
    } else {
        Ok(redirect_flash(
            "/",
            FlashKind::Err,
            "初始設定已由其他管理員完成，這次的內容沒有儲存",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(token_matches("s3cret", "s3cret"));
        assert!(!token_matches("s3cret", "s3cre"));
        assert!(!token_matches("s3cret", "s3creT"));
    }
}
