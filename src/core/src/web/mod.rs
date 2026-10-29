//! HTTP layer shared by both deployments.
//!
//! Only `uri().path()` / `uri().query()` and the `Host` header are used: the Worker
//! receives an absolute request URI while hyper receives `/path`.

mod admin;
mod api;
mod auth;
mod forms;
mod issues;
mod labels;
mod series;
mod setup;
mod site;
mod views;

use std::sync::{Arc, Mutex};

use askama::Template;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};

use crate::db::{DbError, Runtime};
use crate::store;

pub use auth::{Ctx, PwCtx};
pub use site::{Config, Site};

pub struct App<R> {
    pub rt: R,
    pub cfg: Arc<Config>,
    /// Settings change rarely, so they are kept briefly in memory. The Worker builds the
    /// router per request, so there the cache lives for one request.
    site_cache: Arc<Mutex<SiteCache>>,
    /// When this process last looked for series to generate.
    series_run: Arc<Mutex<i64>>,
}

/// The Worker builds the router per request, so there every request checks; the check
/// is one indexed query.
const SERIES_RUN_INTERVAL_MS: i64 = 60_000;

#[derive(Default)]
struct SiteCache {
    site: Option<(Site, i64)>,
    /// Bumped by every save, so a read that started before a save cannot store stale values.
    generation: u64,
}

/// Also picks up changes made outside this process (another container, direct SQL).
const SITE_CACHE_TTL_MS: i64 = 30_000;

impl<R: Clone> Clone for App<R> {
    fn clone(&self) -> Self {
        App {
            rt: self.rt.clone(),
            cfg: self.cfg.clone(),
            site_cache: self.site_cache.clone(),
            series_run: self.series_run.clone(),
        }
    }
}

impl<R: Runtime> App<R> {
    fn cache(&self) -> std::sync::MutexGuard<'_, SiteCache> {
        self.site_cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub async fn site(&self) -> Result<Site, DbError> {
        let now = self.rt.now_ms();
        let generation = {
            let cache = self.cache();
            if let Some((site, loaded_at)) = &cache.site
                && now - loaded_at < SITE_CACHE_TTL_MS
            {
                return Ok(site.clone());
            }
            cache.generation
        };
        let site = Site::resolve(&store::load_settings(&self.rt).await?, &self.cfg);
        let mut cache = self.cache();
        if cache.generation == generation {
            cache.site = Some((site.clone(), now));
        }
        Ok(site)
    }

    /// Generates upcoming occurrences of recurring series. Failures are only logged so
    /// they never break the page being loaded.
    pub async fn generate_series(&self, tz: chrono_tz::Tz) {
        let now = self.rt.now_ms();
        {
            let mut last = self.series_run.lock().unwrap_or_else(|e| e.into_inner());
            if now - *last < SERIES_RUN_INTERVAL_MS {
                return;
            }
            *last = now;
        }
        if let Err(e) = crate::schedule::generate_pending(&self.rt, tz).await {
            self.rt.log_error(&format!("series generation: {e}"));
        }
    }

    /// Call after saving settings so the next request reads them again.
    pub fn forget_site(&self) {
        let mut cache = self.cache();
        cache.site = None;
        cache.generation += 1;
    }
}

pub fn router<R: Runtime>(rt: R, cfg: Config) -> Router {
    let app = App {
        rt,
        cfg: Arc::new(cfg),
        site_cache: Arc::new(Mutex::new(SiteCache::default())),
        series_run: Arc::new(Mutex::new(0)),
    };
    Router::new()
        .route("/healthz", get(healthz::<R>))
        .route("/static/app.css", get(stylesheet))
        .route("/static/app.js", get(script))
        .route("/login", get(auth::login_page::<R>).post(auth::login::<R>))
        .route("/logout", post(auth::logout::<R>))
        .route(
            "/signup",
            get(auth::signup_page::<R>).post(auth::signup::<R>),
        )
        .route(
            "/password",
            get(auth::password_page::<R>).post(auth::change_password::<R>),
        )
        .route("/account/profile", post(auth::change_profile::<R>))
        .route("/setup", get(setup::page::<R>).post(setup::submit::<R>))
        .route("/", get(issues::dashboard::<R>))
        .route("/issues", get(issues::list::<R>))
        .route("/board", get(issues::board::<R>))
        .route("/team", get(issues::team::<R>))
        .route(
            "/issues/new",
            get(issues::new_page::<R>).post(issues::create::<R>),
        )
        .route("/issues/{id}", get(issues::show::<R>))
        .route(
            "/issues/{id}/edit",
            get(issues::edit_page::<R>).post(issues::update::<R>),
        )
        .route("/issues/{id}/status", post(issues::change_status::<R>))
        .route("/issues/{id}/claim", post(issues::claim::<R>))
        .route("/issues/{id}/release", post(issues::release::<R>))
        .route("/issues/{id}/reminder", post(issues::set_reminder::<R>))
        .route("/issues/{id}/delete", post(issues::delete::<R>))
        .route("/series/{id}", get(series::show::<R>))
        .route(
            "/series/{id}/edit",
            get(series::edit_page::<R>).post(series::update::<R>),
        )
        .route("/series/{id}/stop", post(series::stop::<R>))
        .route("/series/{id}/reminder", post(series::set_reminder::<R>))
        .route("/labels", get(labels::page::<R>).post(labels::create::<R>))
        .route("/labels/{id}/delete", post(labels::delete::<R>))
        .route("/admin", get(admin::page::<R>))
        .route("/admin/users", post(admin::create_user::<R>))
        .route("/admin/users/{id}/reset", post(admin::reset_password::<R>))
        .route("/admin/users/{id}/admin", post(admin::toggle_admin::<R>))
        .route("/admin/users/{id}/active", post(admin::toggle_active::<R>))
        .route("/admin/settings", post(admin::settings::<R>))
        .route("/api/reminders", get(api::due_reminders::<R>))
        .route("/api/reminders/{id}/ack", post(api::ack_reminder::<R>))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(app.clone(), log_errors::<R>))
        .layer(middleware::from_fn_with_state(app.clone(), csrf_guard::<R>))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            security_headers::<R>,
        ))
        .with_state(app)
}

async fn healthz<R: Runtime>(State(app): State<App<R>>) -> Response {
    match store::ping(&app.rt).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            // 503 tells load balancers the dependency is down; the extension gets it logged.
            let mut response =
                (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response();
            response.extensions_mut().insert(ErrorDetail(e.to_string()));
            response
        }
    }
}

const STYLESHEET: &str = include_str!("../../static/app.css");
const SCRIPT: &str = include_str!("../../static/app.js");

/// Content hash appended to asset URLs, so browsers may cache them indefinitely and
/// still pick up every change.
pub const ASSET_VERSION: u64 =
    fnv1a(STYLESHEET.as_bytes()) ^ fnv1a(SCRIPT.as_bytes()).rotate_left(1);

const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    hash
}

async fn stylesheet() -> Response {
    static_asset("text/css; charset=utf-8", STYLESHEET)
}

async fn script() -> Response {
    static_asset("text/javascript; charset=utf-8", SCRIPT)
}

fn static_asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}

async fn not_found() -> Response {
    AppError::NotFound.into_response()
}

/// Rejects state-changing requests that come from another site.
async fn csrf_guard<R: Runtime>(State(app): State<App<R>>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        || is_same_origin(req.headers())
    {
        return next.run(req).await;
    }
    // Only clients without Sec-Fetch-Site (whose verdict is final) may fall back to an
    // Origin equal to the configured site URL, for old browsers behind a proxy.
    let headers = req.headers();
    let fallback = header_str(headers, "sec-fetch-site").is_none();
    let origin = header_str(headers, header::ORIGIN.as_str()).map(str::to_owned);
    let configured = match (fallback, origin) {
        (true, Some(origin)) => app.site().await.is_ok_and(|site| site.is_origin(&origin)),
        _ => false,
    };
    if configured {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "跨站請求已被拒絕").into_response()
    }
}

/// Defense in depth on every response: no framing (clickjacking), a strict CSP behind
/// askama's escaping, no MIME sniffing, and no caching of signed-in pages.
async fn security_headers<R: Runtime>(
    State(app): State<App<R>>,
    req: Request,
    next: Next,
) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    let fixed = [
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'self'; \
             frame-ancestors 'none'",
        ),
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "same-origin"),
    ];
    for (name, value) in fixed {
        headers.insert(name, HeaderValue::from_static(value));
    }
    // Static assets set their own long-lived caching; everything else may be private.
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if app.cfg.cookie_secure == Some(true) {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response
}

/// Browsers send `Sec-Fetch-Site`, which survives reverse proxies that rewrite `Host`;
/// older clients fall back to comparing `Origin` with the (forwarded) host.
fn is_same_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        return matches!(site, "same-origin" | "none");
    }
    let Some(origin) = header_str(headers, header::ORIGIN.as_str()) else {
        return true;
    };
    let host = header_str(headers, "x-forwarded-host")
        .or_else(|| header_str(headers, header::HOST.as_str()));
    origin_host(origin).is_some() && origin_host(origin) == host
}

fn origin_host(origin: &str) -> Option<&str> {
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Error details travel in a response extension so they reach the runtime logger.
#[derive(Clone)]
struct ErrorDetail(String);

async fn log_errors<R: Runtime>(State(app): State<App<R>>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let response = next.run(req).await;
    if let Some(ErrorDetail(detail)) = response.extensions().get::<ErrorDetail>() {
        app.rt.log_error(&format!("{method} {path}: {detail}"));
    }
    response
}

pub enum AppError {
    Db(DbError),
    NotFound,
    Forbidden,
}

impl From<DbError> for AppError {
    fn from(e: DbError) -> Self {
        AppError::Db(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Db(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "系統發生錯誤，請稍後再試。",
            ),
            AppError::NotFound => (StatusCode::NOT_FOUND, "找不到這個頁面，或你沒有權限查看。"),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "你沒有權限執行這個操作。"),
        };
        let page = views::ErrorPage {
            asset_version: ASSET_VERSION,
            code: status.as_u16(),
            message,
        };
        let mut response = (status, Html(page.render().unwrap_or_default())).into_response();
        if let AppError::Db(e) = self {
            response.extensions_mut().insert(ErrorDetail(e.to_string()));
        }
        response
    }
}

pub type HandlerResult = Result<Response, AppError>;

// ---------------------------------------------------------------- responses

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
}

fn with_cookie(mut response: Response, cookie: String) -> Response {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

pub fn redirect(to: &str) -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, to.to_owned())]).into_response()
}

/// Redirect target from a form or query; only same-site paths are accepted.
pub fn safe_next(next: &str, fallback: &str) -> String {
    // Browsers drop tabs/newlines and treat `\` like `/`, so "/\t/evil" would become "//evil".
    let plain = !next
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\');
    if plain && next.starts_with('/') && !next.starts_with("//") {
        next.to_owned()
    } else {
        fallback.to_owned()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlashKind {
    Ok,
    Err,
}

pub struct Flash {
    pub kind: &'static str,
    pub text: String,
}

const FLASH_COOKIE: &str = "flash";

pub fn redirect_flash(to: &str, kind: FlashKind, text: &str) -> Response {
    let prefix = if kind == FlashKind::Ok { "ok" } else { "err" };
    let value: String =
        form_urlencoded::byte_serialize(format!("{prefix}:{text}").as_bytes()).collect();
    with_cookie(
        redirect(to),
        format!("{FLASH_COOKIE}={value}; Path=/; Max-Age=60; HttpOnly; SameSite=Lax"),
    )
}

pub fn read_flash(headers: &HeaderMap) -> Option<Flash> {
    let raw = cookie(headers, FLASH_COOKIE)?;
    let decoded: String = form_urlencoded::parse(format!("v={raw}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())?;
    let (kind, text) = decoded.split_once(':')?;
    Some(Flash {
        kind: if kind == "ok" { "ok" } else { "err" },
        text: text.to_owned(),
    })
}

/// Renders a page and clears the flash cookie it displayed.
pub fn render(template: &impl Template, had_flash: bool) -> HandlerResult {
    let html = template
        .render()
        .map_err(|e| AppError::Db(DbError::Other(format!("template: {e}"))))?;
    let response = Html(html).into_response();
    Ok(if had_flash {
        with_cookie(response, format!("{FLASH_COOKIE}=; Path=/; Max-Age=0"))
    } else {
        response
    })
}

fn session_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("sid={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800{secure}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Row, Stmt, Value};
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    /// Answers every query with no rows, or fails everything when `down`.
    #[derive(Clone)]
    struct StubRuntime {
        down: bool,
    }

    impl Runtime for StubRuntime {
        async fn query(&self, _: &str, _: Vec<Value>) -> Result<Vec<Row>, DbError> {
            if self.down {
                Err(DbError::Other("down".into()))
            } else {
                Ok(Vec::new())
            }
        }
        async fn execute(&self, _: &str, _: Vec<Value>) -> Result<u64, DbError> {
            Ok(0)
        }
        async fn batch(&self, _: Vec<Stmt>) -> Result<(), DbError> {
            Ok(())
        }
        fn now_ms(&self) -> i64 {
            0
        }
        fn fill_random(&self, buf: &mut [u8]) {
            buf.fill(7);
        }
        fn log_error(&self, _: &str) {}
    }

    async fn send(down: bool, request: HttpRequest<Body>) -> Response {
        router(StubRuntime { down }, Config::from_vars(|_| None))
            .oneshot(request)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn healthz_reports_database_state() {
        let ok = send(
            false,
            HttpRequest::get("/healthz").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        let down = send(
            true,
            HttpRequest::get("/healthz").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(down.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn anonymous_pages_redirect_to_login() {
        let response = send(
            false,
            HttpRequest::get("/issues?scope=all")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[header::LOCATION],
            "/login?next=%2Fissues%3Fscope%3Dall"
        );
    }

    #[tokio::test]
    async fn post_actions_return_to_the_issue_after_login() {
        let request = HttpRequest::post("/issues/01ARZ3NDEKTSV4RRFFQ69G5FAV/claim")
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .unwrap();
        let response = send(false, request).await;
        assert_eq!(
            response.headers()[header::LOCATION],
            "/login?next=%2Fissues%2F01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
    }

    #[tokio::test]
    async fn setup_requires_login() {
        let response = send(
            false,
            HttpRequest::get("/setup").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(response.headers()[header::LOCATION], "/login?next=%2Fsetup");
    }

    #[tokio::test]
    async fn responses_carry_security_headers() {
        let response = send(
            false,
            HttpRequest::get("/login").body(Body::empty()).unwrap(),
        )
        .await;
        let headers = response.headers();
        assert_eq!(headers["x-frame-options"], "DENY");
        assert_eq!(headers["cache-control"], "no-store");
        assert!(
            headers["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        let css = send(
            false,
            HttpRequest::get("/static/app.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(
            css.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
    }

    #[tokio::test]
    async fn cross_site_posts_are_rejected() {
        let request = HttpRequest::post("/logout")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(false, request).await.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn safe_next_rejects_offsite_targets() {
        assert_eq!(safe_next("/issues?x=1", "/"), "/issues?x=1");
        assert_eq!(safe_next("//evil.example", "/"), "/");
        assert_eq!(safe_next("https://evil.example", "/"), "/");
        assert_eq!(safe_next("/\\evil", "/"), "/");
        assert_eq!(safe_next("/\t/evil.example", "/"), "/");
        assert_eq!(safe_next("/\n/evil.example", "/"), "/");
    }

    #[test]
    fn same_origin_detection() {
        let headers = |pairs: &[(&'static str, &str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, HeaderValue::from_str(value).unwrap());
            }
            map
        };
        assert!(is_same_origin(&headers(&[(
            "sec-fetch-site",
            "same-origin"
        )])));
        assert!(!is_same_origin(&headers(&[
            ("sec-fetch-site", "cross-site"),
            ("origin", "http://a"),
            ("host", "a")
        ])));
        assert!(!is_same_origin(&headers(&[(
            "sec-fetch-site",
            "same-site"
        )])));
        // Behind a proxy that rewrites Host, the forwarded host is used.
        assert!(is_same_origin(&headers(&[
            ("origin", "https://todo.example"),
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "todo.example"),
        ])));
        assert!(!is_same_origin(&headers(&[
            ("origin", "https://evil.example"),
            ("host", "todo.example")
        ])));
        assert!(!is_same_origin(&headers(&[
            ("origin", "null"),
            ("host", "todo.example")
        ])));
        assert!(is_same_origin(&headers(&[])));
    }

    #[test]
    fn origin_host_strips_scheme() {
        assert_eq!(
            origin_host("https://todo.example:8443"),
            Some("todo.example:8443")
        );
        assert_eq!(origin_host("null"), None);
    }

    #[test]
    fn cookie_parsing_and_flash_round_trip() {
        let response = redirect_flash("/", FlashKind::Ok, "已儲存 a;b");
        let set = response.headers()[header::SET_COOKIE].to_str().unwrap();
        let value = set.split(';').next().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("sid=abc; {value}")).unwrap(),
        );
        assert_eq!(cookie(&headers, "sid").as_deref(), Some("abc"));
        let flash = read_flash(&headers).unwrap();
        assert_eq!(flash.kind, "ok");
        assert_eq!(flash.text, "已儲存 a;b");
    }
}
