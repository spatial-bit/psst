use crate::InvalidValue;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnixMillis(i64);

impl UnixMillis {
    /// Creates a timestamp at or after the Unix epoch.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for a negative epoch value.
    pub fn new(value: i64) -> Result<Self, InvalidValue> {
        if value < 0 {
            return Err(InvalidValue::new(
                "timestamp",
                "must not precede the Unix epoch",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self.0
    }

    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        let millis = i64::try_from(duration.as_millis()).ok()?;
        self.0.checked_add(millis).map(Self)
    }
}

pub trait Clock {
    fn now(&self) -> UnixMillis;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_checked() {
        assert!(UnixMillis::new(-1).is_err());
        assert_eq!(
            UnixMillis::new(1)
                .unwrap()
                .checked_add(Duration::from_millis(2))
                .unwrap()
                .as_i64(),
            3
        );
        assert!(
            UnixMillis::new(i64::MAX)
                .unwrap()
                .checked_add(Duration::from_millis(1))
                .is_none()
        );
    }
}
