use crate::InvalidValue;
use std::fmt;

macro_rules! text_value {
    ($name:ident, $field:literal, $max:expr, $validator:expr, $reason:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);
        impl $name {
            /// Creates a validated domain value.
            ///
            /// # Errors
            ///
            /// Returns [`InvalidValue`] when the value violates this type's constraints.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
                let value = value.into();
                if value.is_empty() {
                    return Err(InvalidValue::new($field, "must not be empty"));
                }
                if value.len() > $max {
                    return Err(InvalidValue::new($field, "is too large"));
                }
                if !($validator)(&value) {
                    return Err(InvalidValue::new($field, $reason));
                }
                Ok(Self(value))
            }
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::str::FromStr for $name {
            type Err = InvalidValue;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

fn valid_squad_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_member_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}

fn trimmed_nonempty(value: &str) -> bool {
    value.trim() == value && !value.trim().is_empty()
}
fn printable(value: &str) -> bool {
    trimmed_nonempty(value) && !value.chars().any(char::is_control)
}

text_value!(
    SquadName,
    "squad_name",
    64,
    valid_squad_name,
    "must be a lowercase ASCII routing name"
);
text_value!(
    MemberName,
    "member_name",
    64,
    valid_member_name,
    "must be a lowercase ASCII routing name"
);
text_value!(
    Mission,
    "mission",
    4096,
    trimmed_nonempty,
    "must not have surrounding whitespace"
);
text_value!(
    Role,
    "role",
    256,
    trimmed_nonempty,
    "must not have surrounding whitespace"
);
text_value!(
    DedupeKey,
    "dedupe_key",
    256,
    printable,
    "must contain printable text without surrounding whitespace"
);
text_value!(
    CorrelationId,
    "correlation_id",
    256,
    printable,
    "must contain printable text without surrounding whitespace"
);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MessageBody(String);

impl MessageBody {
    pub const MAX_BYTES: usize = 64 * 1024;

    /// Creates a non-empty message body within the protocol byte limit.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] when the body is empty or exceeds 64 KiB.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidValue::new("message_body", "must not be empty"));
        }
        if value.len() > Self::MAX_BYTES {
            return Err(InvalidValue::new("message_body", "is too large"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_names_are_unambiguous() {
        for invalid in ["", " Alpha", "alpha ", "Alpha", "álpha", "-alpha", "alpha-"] {
            assert!(SquadName::new(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(SquadName::new("alpha-2").is_ok());
        assert!(MemberName::new("critic_2.eu").is_ok());
    }

    #[test]
    fn rich_text_is_byte_bounded() {
        assert!(Mission::new("λ mission").is_ok());
        assert!(Mission::new(" mission").is_err());
        assert!(Role::new("  ").is_err());
        assert!(MessageBody::new("x".repeat(MessageBody::MAX_BYTES)).is_ok());
        assert!(MessageBody::new("x".repeat(MessageBody::MAX_BYTES + 1)).is_err());
        assert!(MessageBody::new("🦀".repeat(MessageBody::MAX_BYTES / 4)).is_ok());
        assert!(MessageBody::new("🦀".repeat(MessageBody::MAX_BYTES / 4 + 1)).is_err());
    }

    #[test]
    fn every_text_value_enforces_its_boundaries() {
        assert!(SquadName::new("a".repeat(64)).is_ok());
        assert!(SquadName::new("a".repeat(65)).is_err());
        assert!(MemberName::new("a".repeat(64)).is_ok());
        assert!(MemberName::new("a".repeat(65)).is_err());

        assert!(Mission::new("x".repeat(4096)).is_ok());
        assert!(Mission::new("x".repeat(4097)).is_err());
        assert!(Mission::new("").is_err());
        assert!(Role::new("x".repeat(256)).is_ok());
        assert!(Role::new("x".repeat(257)).is_err());
        assert!(Role::new("\t").is_err());

        macro_rules! assert_printable_boundaries {
            ($type:ty) => {
                assert!(<$type>::new("x".repeat(256)).is_ok());
                assert!(<$type>::new("x".repeat(257)).is_err());
                assert!(<$type>::new("").is_err());
                assert!(<$type>::new(" surrounded ").is_err());
                assert!(<$type>::new("line\nbreak").is_err());
                assert!(<$type>::new("λ-key").is_ok());
            };
        }
        assert_printable_boundaries!(DedupeKey);
        assert_printable_boundaries!(CorrelationId);
    }

    #[test]
    fn message_content_is_preserved_exactly() {
        let body = MessageBody::new("  exact\n").unwrap();
        assert_eq!(body.as_str(), "  exact\n");
        assert!(MessageBody::new("").is_err());
    }
}
