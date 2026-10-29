use argon2::{Argon2, PasswordHasher, PasswordVerifier};

pub const MIN_LEN: usize = 6;

/// Verified against when the account does not exist, so login timing does not reveal it.
pub const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$dGVhbS10b2RvLWFkbWluIQ$j8qFu0U8Mpixu+y1mLCbjU409z+hXfCnJ9/3gn/+qiU";

/// At least `MIN_LEN` characters and at least two of: upper, lower, digit, symbol.
/// Only ASCII counts toward a class, so e.g. CJK characters add length but no class.
pub fn check_policy(password: &str) -> Result<(), &'static str> {
    if password.chars().count() < MIN_LEN {
        return Err("密碼長度至少 6 個字元");
    }
    let classes = [
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| c.is_ascii_punctuation()),
    ];
    if classes.iter().filter(|present| **present).count() < 2 {
        return Err("密碼須包含大寫英文、小寫英文、數字、符號（如 !@#$）中的至少兩種");
    }
    Ok(())
}

pub fn hash(password: &str, salt: &[u8; 16]) -> String {
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), salt)
        .expect("argon2 with default params and a 16-byte salt cannot fail")
        .to_string()
}

pub fn verify(password: &str, hash: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_requires_length_and_two_classes() {
        assert!(check_policy("Ab1").is_err());
        assert!(check_policy("abcdef").is_err());
        assert!(check_policy("123456").is_err());
        assert!(check_policy("abc123").is_ok());
        assert!(check_policy("ABCDE!").is_ok());
        assert!(check_policy("Admin@12345").is_ok());
        assert!(
            check_policy("中文密碼測試!").is_err(),
            "CJK is not a class; only the symbol class"
        );
        assert!(check_policy("密碼密碼密碼1").is_err(), "digits only");
        assert!(check_policy("abcdeé").is_err(), "lowercase only");
        assert!(check_policy("密碼密碼a1").is_ok());
    }

    #[test]
    fn hash_round_trips() {
        let h = hash("Secret1!", &[7; 16]);
        assert!(verify("Secret1!", &h));
        assert!(!verify("secret1!", &h));
        assert!(!verify("Secret1!", "not-a-hash"));
    }

    /// Generates the seeded admin hash in 0001_init.sql:
    /// `cargo test -p todo-core print_admin_hash -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn print_admin_hash() {
        println!("{}", hash("Admin@12345", b"team-todo-admin!"));
    }
}
