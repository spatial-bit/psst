use crate::InvalidValue;
use std::{fmt, str::FromStr};

macro_rules! opaque_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Creates a validated opaque identifier.
            ///
            /// # Errors
            ///
            /// Returns [`InvalidValue`] when the prefix, length, or character set is invalid.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
                let value = value.into();
                let Some(suffix) = value.strip_prefix(concat!($prefix, "_")) else {
                    return Err(InvalidValue::new(stringify!($name), "has the wrong prefix"));
                };
                if suffix.is_empty() || value.len() > 128 {
                    return Err(InvalidValue::new(
                        stringify!($name),
                        "has an invalid length",
                    ));
                }
                if !suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                {
                    return Err(InvalidValue::new(
                        stringify!($name),
                        "contains invalid characters",
                    ));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = InvalidValue;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

opaque_id!(SquadId, "sqd");
opaque_id!(AgentId, "agt");
opaque_id!(MembershipId, "mem");
opaque_id!(InstanceId, "ins");
opaque_id!(MessageId, "msg");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_typed() {
        assert!(SquadId::new("sqd_01ab-cd").is_ok());
        assert!(SquadId::new("msg_01ab").is_err());
        assert!(MessageId::new("msg_01AB").is_err());
        assert!(AgentId::new("agt_").is_err());
    }

    #[test]
    fn ids_enforce_character_and_total_length_boundaries() {
        assert!(SquadId::new(format!("sqd_{}", "a".repeat(124))).is_ok());
        assert!(SquadId::new(format!("sqd_{}", "a".repeat(125))).is_err());
        for invalid in ["sqd_a_b", "sqd_a.b", "sqd_á", "sqd_A"] {
            assert!(SquadId::new(invalid).is_err(), "accepted {invalid:?}");
        }
    }
}
