//! Site-wide settings: stored in the database, with the deployment's configuration
//! supplying the initial values.

use chrono_tz::Tz;

use super::forms::FormData;
use super::views::{Opt, SiteForm};
use crate::store::{SiteUpdate, StoredSettings};

pub const SITE_NAME_MAX: usize = 32;
pub const DEFAULT_SITE_NAME: &str = "Team";

/// Values from `.env` (Docker) or `[vars]` (Worker); they only seed the settings.
pub struct Config {
    pub site_name: String,
    pub site_url: String,
    pub tz: Tz,
    /// Explicit `COOKIE_SECURE`; when unset, an `https://` site URL decides.
    pub cookie_secure: Option<bool>,
    /// Optional `SETUP_TOKEN` the first-run setup page asks for, so knowing the published
    /// default password alone is not enough to take over a fresh deployment.
    pub setup_token: Option<String>,
}

impl Config {
    /// Reads `SITE_NAME`, `SITE_URL`, `APP_TIMEZONE`, `COOKIE_SECURE` and `SETUP_TOKEN`.
    /// Invalid values fall back to their defaults rather than stopping the site.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let non_empty = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Config {
            site_name: non_empty("SITE_NAME")
                .and_then(|name| site_name(&name).ok())
                .unwrap_or_else(|| DEFAULT_SITE_NAME.to_owned()),
            site_url: non_empty("SITE_URL")
                .and_then(|url| normalize_url(&url).ok())
                .unwrap_or_default(),
            tz: non_empty("APP_TIMEZONE")
                .and_then(|name| name.parse().ok())
                .unwrap_or(chrono_tz::Asia::Taipei),
            cookie_secure: non_empty("COOKIE_SECURE").and_then(|v| parse_flag(&v)),
            setup_token: non_empty("SETUP_TOKEN"),
        }
    }
}

fn parse_flag(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Effective settings for one request.
#[derive(Debug, Clone)]
pub struct Site {
    /// Brand shown before "TODO".
    pub name: String,
    /// `scheme://host[:port]`, or empty when unknown.
    pub url: String,
    pub tz: Tz,
    pub allow_signup: bool,
    pub setup_completed: bool,
    pub cookie_secure: bool,
}

impl Site {
    pub fn resolve(stored: &StoredSettings, cfg: &Config) -> Self {
        let pick = |value: &str, fallback: &str| {
            if value.is_empty() {
                fallback.to_owned()
            } else {
                value.to_owned()
            }
        };
        let url = pick(&stored.site_url, &cfg.site_url);
        Site {
            name: pick(&stored.site_name, &cfg.site_name),
            tz: stored.timezone.parse().unwrap_or(cfg.tz),
            allow_signup: stored.allow_signup,
            setup_completed: stored.setup_completed,
            cookie_secure: cfg.cookie_secure.unwrap_or(url.starts_with("https://")),
            url,
        }
    }

    /// True when the browser's `Origin` is the configured site URL (reverse proxies).
    pub fn is_origin(&self, origin: &str) -> bool {
        !self.url.is_empty() && origin.eq_ignore_ascii_case(&self.url)
    }
}

impl SiteForm {
    pub fn from_site(site: &Site) -> Self {
        SiteForm {
            site_name: site.name.clone(),
            site_url: site.url.clone(),
            timezone: site.tz.name().to_owned(),
            allow_signup: site.allow_signup,
        }
    }

    pub fn from_post(form: &FormData) -> Self {
        SiteForm {
            site_name: form.get("site_name").to_owned(),
            site_url: form.get("site_url").to_owned(),
            timezone: form.get("timezone").to_owned(),
            allow_signup: form.checked("allow_signup"),
        }
    }

    /// Validated values ready to store, or every problem found.
    pub fn validate(&self) -> Result<SiteUpdate, Vec<String>> {
        let mut errors = Vec::new();
        let mut keep = |result: Result<String, &'static str>| {
            result.unwrap_or_else(|e| {
                errors.push(e.to_owned());
                String::new()
            })
        };
        let update = SiteUpdate {
            site_name: keep(site_name(&self.site_name)),
            site_url: keep(normalize_url(&self.site_url)),
            timezone: keep(timezone(&self.timezone)),
            allow_signup: self.allow_signup,
        };
        if errors.is_empty() {
            Ok(update)
        } else {
            Err(errors)
        }
    }
}

/// An `https://` site URL turns on Secure cookies; saving one while the browser is on
/// plain http would make every later login cookie disappear, so refuse that.
pub fn https_unreachable(url: &str, headers: &axum::http::HeaderMap) -> bool {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let over_https = header("x-forwarded-proto") == Some("https")
        || header("origin").is_some_and(|o| o.starts_with("https://"));
    let known_http = header("origin").is_some_and(|o| o.starts_with("http://"));
    url.starts_with("https://") && known_http && !over_https
}

pub const HTTPS_UNREACHABLE: &str = "目前是以 http 連線，若把網站網址設為 https://，登入 Cookie 會加上 Secure 而無法使用 http 登入。請先設定好 HTTPS 再改，或改用 http:// 網址。";

pub fn site_name(input: &str) -> Result<String, &'static str> {
    let name = input.trim();
    if name.is_empty() || name.chars().count() > SITE_NAME_MAX {
        return Err("品牌名稱須為 1～32 個字元");
    }
    Ok(name.to_owned())
}

/// Accepts `http(s)://host[:port]` (a trailing `/` and default ports are dropped, so the
/// value compares equal to a browser's `Origin`); empty means "not set".
pub fn normalize_url(input: &str) -> Result<String, &'static str> {
    let url = input.trim().trim_end_matches('/').to_ascii_lowercase();
    if url.is_empty() {
        return Ok(String::new());
    }
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err("網站網址須以 http:// 或 https:// 開頭");
    };
    let (host, port) = split_host_port(rest).ok_or(INVALID_HOST)?;
    if !valid_host(host) {
        return Err(INVALID_HOST);
    }
    Ok(match port {
        Some(p) if !(scheme == "https" && p == 443 || scheme == "http" && p == 80) => {
            format!("{scheme}://{host}:{p}")
        }
        _ => format!("{scheme}://{host}"),
    })
}

const INVALID_HOST: &str = "網站網址只填到網域與 port，例如 https://todo.example.com";

fn split_host_port(rest: &str) -> Option<(&str, Option<u16>)> {
    if rest.starts_with('[') {
        let end = rest.find(']')?;
        let (host, tail) = rest.split_at(end + 1);
        return match tail.strip_prefix(':') {
            Some(port) => Some((host, Some(port.parse().ok()?))),
            None if tail.is_empty() => Some((host, None)),
            None => None,
        };
    }
    match rest.rsplit_once(':') {
        Some((host, port)) => Some((host, Some(port.parse().ok()?))),
        None => Some((rest, None)),
    }
}

/// A DNS name, an IPv4 address or a bracketed IPv6 address.
fn valid_host(host: &str) -> bool {
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<std::net::Ipv6Addr>().is_ok();
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

pub fn timezone(input: &str) -> Result<String, &'static str> {
    input
        .trim()
        .parse::<Tz>()
        .map(|tz| tz.name().to_owned())
        .map_err(|_| "請選擇有效的時區")
}

/// Every IANA zone, so any deployment location can be chosen.
pub fn timezone_options(current: &str) -> Vec<Opt> {
    let mut names: Vec<&str> = chrono_tz::TZ_VARIANTS.iter().map(|tz| tz.name()).collect();
    names.sort_unstable();
    names
        .into_iter()
        .map(|name| Opt::new(name, name, current))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::from_vars(|name| match name {
            "SITE_NAME" => Some("Acme".into()),
            "APP_TIMEZONE" => Some("Not/AZone".into()),
            _ => None,
        })
    }

    #[test]
    fn config_reads_initial_values() {
        let cfg = config();
        assert_eq!(cfg.site_name, "Acme");
        assert_eq!(
            cfg.tz,
            chrono_tz::Asia::Taipei,
            "unknown zones fall back to Taipei"
        );
        assert_eq!(cfg.cookie_secure, None);
        assert_eq!(cfg.setup_token, None);
        let flags = Config::from_vars(|name| match name {
            "COOKIE_SECURE" => Some("yes".into()),
            "SITE_NAME" => Some("x".repeat(40)),
            _ => None,
        });
        assert_eq!(flags.cookie_secure, Some(true));
        assert_eq!(
            flags.site_name, DEFAULT_SITE_NAME,
            "too long names fall back"
        );
        let defaults = Config::from_vars(|_| None);
        assert_eq!(defaults.site_name, DEFAULT_SITE_NAME);
    }

    #[test]
    fn stored_values_override_initial_ones() {
        let stored = StoredSettings {
            site_name: "Acme QA".into(),
            site_url: "https://todo.example.com".into(),
            timezone: "UTC".into(),
            allow_signup: true,
            setup_completed: true,
        };
        let site = Site::resolve(&stored, &config());
        assert_eq!(site.name, "Acme QA");
        assert_eq!(site.tz, chrono_tz::UTC);
        assert!(site.cookie_secure, "https URL turns on Secure cookies");
        assert!(site.is_origin("https://TODO.example.com"));

        let empty = Site::resolve(&StoredSettings::default(), &config());
        assert_eq!(empty.name, "Acme");
        assert!(!empty.cookie_secure);
        assert!(!empty.is_origin("https://todo.example.com"));
    }

    #[test]
    fn explicit_cookie_setting_wins() {
        let cfg = Config::from_vars(|name| (name == "COOKIE_SECURE").then(|| "false".into()));
        let stored = StoredSettings {
            site_url: "https://todo.example.com".into(),
            ..StoredSettings::default()
        };
        assert!(!Site::resolve(&stored, &cfg).cookie_secure);
    }

    #[test]
    fn url_validation() {
        assert_eq!(
            normalize_url(" https://Todo.Example.com/ "),
            Ok("https://todo.example.com".into())
        );
        assert_eq!(
            normalize_url("http://192.168.1.5:8080"),
            Ok("http://192.168.1.5:8080".into())
        );
        assert_eq!(
            normalize_url("https://todo.example.com:443"),
            Ok("https://todo.example.com".into())
        );
        assert_eq!(
            normalize_url("http://[::1]:8080"),
            Ok("http://[::1]:8080".into())
        );
        assert!(normalize_url("https://...").is_err());
        assert!(normalize_url("https://a:b:c").is_err());
        assert!(normalize_url("https://[").is_err());
        assert_eq!(normalize_url(""), Ok(String::new()));
        assert!(normalize_url("todo.example.com").is_err());
        assert!(normalize_url("https://todo.example.com/app").is_err());
        assert!(normalize_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn name_and_timezone_validation() {
        assert_eq!(site_name("  Acme "), Ok("Acme".into()));
        assert!(site_name("").is_err());
        assert_eq!(timezone("Asia/Taipei"), Ok("Asia/Taipei".into()));
        assert!(timezone("Mars/Base").is_err());
        assert!(timezone_options("Asia/Taipei").iter().any(|o| o.selected));
    }
}
