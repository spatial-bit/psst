use crate::InvalidValue;
use std::fmt;

/// Opaque continuity material. It deliberately has no `Display` or serialization support.
#[derive(Clone, Eq, PartialEq)]
pub struct ResumeToken(String);

impl ResumeToken {
    /// Creates validated opaque resume material.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] when the encoded token is too short, too long, or malformed.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
        let value = value.into();
        if value.len() < 22 {
            return Err(InvalidValue::new(
                "resume_token",
                "must contain at least 128 bits of encoded entropy",
            ));
        }
        if value.len() > 512
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(InvalidValue::new("resume_token", "has invalid encoding"));
        }
        Ok(Self(value))
    }

    /// Exposes the secret only to code that explicitly requests secret material.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResumeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResumeToken([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_token() {
        let secret = "a-very-secret-resume-token";
        let token = ResumeToken::new(secret).unwrap();
        let debug = format!("{token:?}");
        assert_eq!(debug, "ResumeToken([REDACTED])");
        assert!(!debug.contains(secret));
        assert_eq!(token.expose_secret(), secret);
    }

    #[test]
    fn tokens_enforce_encoding_and_length_boundaries() {
        assert!(ResumeToken::new("a".repeat(21)).is_err());
        assert!(ResumeToken::new("a".repeat(22)).is_ok());
        assert!(ResumeToken::new("a".repeat(512)).is_ok());
        assert!(ResumeToken::new("a".repeat(513)).is_err());
        assert!(ResumeToken::new(format!("{}+", "a".repeat(21))).is_err());
        assert!(ResumeToken::new(format!("{}-", "a".repeat(21))).is_ok());
        assert!(ResumeToken::new(format!("{}_", "a".repeat(21))).is_ok());
    }
}
