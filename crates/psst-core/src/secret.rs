use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::fmt;

use crate::InvalidValue;

const TOKEN_BYTES: usize = 32;
const TOKEN_ENCODED_LENGTH: usize = 43;

/// Opaque continuity material. It deliberately has no `Display` or serialization support.
#[derive(Clone, Eq, PartialEq)]
pub struct ResumeToken(String);

impl ResumeToken {
    /// Generates a canonical 256-bit token using the operating system CSPRNG.
    ///
    /// # Errors
    ///
    /// Returns an error if secure operating-system randomness is unavailable.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// Parses a persisted canonical base64url-no-pad 256-bit token.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] unless the input is the unique 43-character
    /// encoding of exactly 32 bytes.
    pub fn from_encoded(value: impl Into<String>) -> Result<Self, InvalidValue> {
        let value = value.into();
        if value.len() != TOKEN_ENCODED_LENGTH {
            return Err(InvalidValue::new(
                "resume_token",
                "must be a canonical 256-bit token",
            ));
        }
        let decoded = URL_SAFE_NO_PAD.decode(&value).map_err(|_| {
            InvalidValue::new(
                "resume_token",
                "must be canonical base64url without padding",
            )
        })?;
        if decoded.len() != TOKEN_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != value {
            return Err(InvalidValue::new(
                "resume_token",
                "must be a canonical 256-bit token",
            ));
        }
        Ok(Self(value))
    }

    /// Exposes the secret only to code that explicitly requests secret material.
    #[must_use]
    pub fn expose_encoded(&self) -> &str {
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
        let secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let token = ResumeToken::from_encoded(secret).unwrap();
        let debug = format!("{token:?}");
        assert_eq!(debug, "ResumeToken([REDACTED])");
        assert!(!debug.contains(secret));
        assert_eq!(token.expose_encoded(), secret);
    }

    #[test]
    fn tokens_are_generated_and_parsed_only_in_canonical_256_bit_form() {
        let generated = ResumeToken::generate().unwrap();
        assert_eq!(generated.expose_encoded().len(), TOKEN_ENCODED_LENGTH);
        assert!(ResumeToken::from_encoded(generated.expose_encoded()).is_ok());
        assert!(ResumeToken::from_encoded("a".repeat(42)).is_err());
        assert!(ResumeToken::from_encoded("a".repeat(44)).is_err());
        assert!(ResumeToken::from_encoded(format!("{}=", "a".repeat(42))).is_err());
        assert!(ResumeToken::from_encoded(format!("{}+", "a".repeat(42))).is_err());
        assert!(ResumeToken::from_encoded("___________________________________________").is_err());
    }
}
