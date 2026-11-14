//! Masking for the Environment tab (spec Phase 1): values that look secret are hidden until the
//! user clicks to reveal them.

/// Whether an environment value may be shown as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Masked {
    Shown,
    Hidden,
}

/// Name fragments (case-insensitive) that mark a variable as secret.
pub const SECRET_NAMES: [&str; 9] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "KEY",
    "AUTH",
    "CREDENTIAL",
    "COOKIE",
    "SESSION",
];

/// Value prefixes of well-known token formats.
pub const SECRET_PREFIXES: [&str; 5] = ["ghp_", "gho_", "github_pat_", "sk-", "xox"];

/// Decides whether `name=value` is shown or hidden.
#[must_use]
pub fn mask(name: &str, value: &str) -> Masked {
    let upper = name.to_ascii_uppercase();
    let secret = SECRET_NAMES.iter().any(|n| upper.contains(n))
        || SECRET_PREFIXES.iter().any(|p| value.starts_with(p))
        || looks_like_jwt(value);
    if secret {
        Masked::Hidden
    } else {
        Masked::Shown
    }
}

/// Like [`mask`], with extra name fragments (case-insensitive) from the settings.
#[must_use]
pub fn mask_with(name: &str, value: &str, extra: &[String]) -> Masked {
    let upper = name.to_uppercase();
    let extra_hit = extra
        .iter()
        .any(|e| !e.is_empty() && upper.contains(&e.to_uppercase()));
    if extra_hit {
        Masked::Hidden
    } else {
        mask(name, value)
    }
}

/// `eyJ…`.`…`.`…`: three non-empty base64url segments, the header starting with `{"`.
fn looks_like_jwt(v: &str) -> bool {
    let mut parts = v.split('.');
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
    };
    v.starts_with("eyJ")
        && parts.next().is_some_and(segment_ok)
        && parts.next().is_some_and(segment_ok)
        && parts.next().is_some_and(segment_ok)
        && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_by_name_table() {
        for name in [
            "GITHUB_TOKEN",
            "aws_secret_access_key",
            "DB_PASSWORD",
            "MYSQL_PASSWD",
            "API_KEY",
            "NPM_AUTH",
            "AZURE_CREDENTIALS",
            "COOKIE_JAR",
            "SESSION_ID",
        ] {
            assert_eq!(mask(name, "x"), Masked::Hidden, "{name}");
        }
        for name in ["PATH", "USERNAME", "NODE_ENV", "PORT", "TEMP"] {
            assert_eq!(mask(name, "x"), Masked::Shown, "{name}");
        }
    }

    #[test]
    fn mask_by_value_table() {
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        for value in [
            jwt,
            "ghp_0123456789abcdefABCDEF0123456789abcd",
            "gho_abc",
            "github_pat_11ABC",
            "sk-proj-abc123",
            "xoxb-123-456",
            "xoxp-1",
        ] {
            assert_eq!(mask("SOMETHING", value), Masked::Hidden, "{value}");
        }
    }

    #[test]
    fn extra_names_from_settings_also_mask() {
        let extra = vec!["internal".to_owned()];
        assert_eq!(mask_with("MY_INTERNAL_URL", "x", &extra), Masked::Hidden);
        assert_eq!(mask_with("PORT", "3000", &extra), Masked::Shown);
        assert_eq!(mask_with("API_KEY", "x", &[]), Masked::Hidden);
    }

    #[test]
    fn mask_leaves_ordinary_values_alone() {
        for value in [
            r"C:\Windows\system32;C:\Program Files\nodejs",
            "development",
            "3000",
            "eyJ-not-a-jwt",
            "skeleton",
        ] {
            assert_eq!(mask("SOMETHING", value), Masked::Shown, "{value}");
        }
    }
}
