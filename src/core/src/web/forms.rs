//! Form decoding and input validation shared by the handlers.

use crate::domain::{DISPLAY_NAME_MAX, LABEL_MAX};

/// Raw `application/x-www-form-urlencoded` pairs; keeps repeated keys (checkbox groups).
pub struct FormData(pub Vec<(String, String)>);

impl FormData {
    pub fn get(&self, key: &str) -> &str {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    }

    pub fn all(&self, key: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub fn checked(&self, key: &str) -> bool {
        !self.get(key).is_empty()
    }
}

/// Usernames are stored lowercase so both databases compare them the same way.
pub fn normalize_username(input: &str) -> Result<String, &'static str> {
    let name = input.trim().to_lowercase();
    if !(3..=32).contains(&name.len()) {
        return Err("帳號長度須為 3～32 個字元");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("帳號只能使用英文字母、數字與 . _ -");
    }
    Ok(name)
}

pub fn display_name(input: &str) -> Result<String, &'static str> {
    let name = input.trim();
    if name.is_empty() || name.chars().count() > DISPLAY_NAME_MAX {
        return Err("顯示名稱須為 1～32 個字元");
    }
    Ok(name.to_owned())
}

/// Loose syntax check: the address is only stored for future notifications.
pub fn email(input: &str) -> Result<String, &'static str> {
    let email = input.trim();
    let shape_ok = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
        });
    if shape_ok {
        Ok(email.to_owned())
    } else {
        Err("Email 格式不正確")
    }
}

pub fn optional_email(input: &str) -> Result<Option<String>, &'static str> {
    if input.trim().is_empty() {
        Ok(None)
    } else {
        email(input).map(Some)
    }
}

pub fn label_name(input: &str) -> Result<String, &'static str> {
    let name = input.trim();
    if name.is_empty() || name.chars().count() > LABEL_MAX {
        return Err("標籤名稱須為 1～32 個字元");
    }
    Ok(name.to_owned())
}

pub fn color(input: &str) -> Result<String, &'static str> {
    let c = input.trim().to_lowercase();
    if c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(c)
    } else {
        Err("顏色格式須為 #rrggbb")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_normalized() {
        assert_eq!(normalize_username("  Alice.W "), Ok("alice.w".into()));
        assert!(normalize_username("ab").is_err());
        assert!(normalize_username("bad name").is_err());
        assert!(normalize_username("中文名").is_err());
    }

    #[test]
    fn form_keeps_repeated_keys() {
        let f = FormData(vec![
            ("labels".into(), "a".into()),
            ("title".into(), "t".into()),
            ("labels".into(), "b".into()),
        ]);
        assert_eq!(f.all("labels"), vec!["a", "b"]);
        assert_eq!(f.get("title"), "t");
        assert_eq!(f.get("missing"), "");
    }

    #[test]
    fn colors_and_names() {
        assert_eq!(color("#ABCDEF"), Ok("#abcdef".into()));
        assert!(color("red").is_err());
        assert!(label_name("   ").is_err());
        assert_eq!(display_name(" 王小明 "), Ok("王小明".into()));
        assert_eq!(email(" admin@example.com "), Ok("admin@example.com".into()));
        assert!(email("admin@localhost").is_err());
        assert!(email("a b@example.com").is_err());
        assert_eq!(optional_email(""), Ok(None));
    }
}
