use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use utoipa::ToSchema;

/// UTC RFC 3339 timestamp with exactly millisecond precision on the wire.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd, ToSchema)]
#[schema(value_type = String, format = DateTime, pattern = r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")]
pub struct ApiTimestamp(OffsetDateTime);

impl ApiTimestamp {
    /// Creates a timestamp after normalizing it to UTC and millisecond precision.
    ///
    /// # Errors
    /// Returns an error when the value contains sub-millisecond precision.
    pub fn new(value: OffsetDateTime) -> Result<Self, &'static str> {
        if value.nanosecond() % 1_000_000 != 0 {
            return Err("timestamp must have exactly millisecond precision");
        }
        Ok(Self(value.to_offset(time::UtcOffset::UTC)))
    }

    #[must_use]
    pub const fn value(self) -> OffsetDateTime {
        self.0
    }
}

impl fmt::Debug for ApiTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApiTimestamp")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for ApiTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.0;
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            value.year(),
            u8::from(value.month()),
            value.day(),
            value.hour(),
            value.minute(),
            value.second(),
            value.millisecond()
        )
    }
}

impl Serialize for ApiTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ApiTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = String::deserialize(deserializer)?;
        if input.len() != 24 || !input.ends_with('Z') || input.as_bytes().get(19) != Some(&b'.') {
            return Err(de::Error::custom("expected UTC RFC 3339 with milliseconds"));
        }
        let parsed = OffsetDateTime::parse(&input, &Rfc3339).map_err(de::Error::custom)?;
        Self::new(parsed).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_contract_is_exact() {
        let timestamp: ApiTimestamp = serde_json::from_str("\"2026-08-07T01:02:03.004Z\"").unwrap();
        assert_eq!(
            serde_json::to_string(&timestamp).unwrap(),
            "\"2026-08-07T01:02:03.004Z\""
        );
        for invalid in [
            "\"2026-08-07T01:02:03Z\"",
            "\"2026-08-07T01:02:03.0000Z\"",
            "\"2026-08-07T01:02:03.000+00:00\"",
        ] {
            assert!(serde_json::from_str::<ApiTimestamp>(invalid).is_err());
        }
    }
}
