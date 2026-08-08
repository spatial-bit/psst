use http::{
    HeaderMap, HeaderValue,
    header::{AUTHORIZATION, CACHE_CONTROL},
};
use psst_core::{InstanceId, InvalidValue, ResumeToken};
use std::fmt;

pub const AUTHORIZATION_HEADER: &str = "authorization";
pub const SESSION_CREDENTIAL_HEADER: &str = "psst-session-credential";
pub const CACHE_CONTROL_HEADER: &str = "cache-control";
pub const NO_STORE: &str = "no-store";
pub const BEARER_PREFIX: &str = "Bearer ";
pub const MAX_CREDENTIAL_LENGTH: usize = 172;

/// Header names whose values middleware and access logs must treat as sensitive.
pub const SENSITIVE_HEADERS: [&str; 2] = [AUTHORIZATION_HEADER, SESSION_CREDENTIAL_HEADER];

/// Produces the only value permitted in access logs for sensitive headers.
#[must_use]
pub fn header_value_for_log(name: &str, value: &str) -> String {
    if SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
    {
        "[REDACTED]".into()
    } else {
        value.into()
    }
}

/// Adapter-only session authority. Deliberately has no serialization or `Display` support.
pub struct SessionCredential {
    instance_id: InstanceId,
    resume_token: ResumeToken,
}

/// Sensitive response headers returned exactly once by join and resume.
pub struct IssuedSessionHeaders {
    credential: HeaderValue,
}

impl IssuedSessionHeaders {
    /// Constructs sensitive/no-store issuance headers from adapter-only authority.
    ///
    /// # Errors
    /// Returns an invalid-value error if the credential cannot be encoded as an HTTP header.
    pub fn new(credential: &SessionCredential) -> Result<Self, InvalidValue> {
        let mut value =
            HeaderValue::from_str(&credential.encoded()).map_err(|_| invalid_credential())?;
        value.set_sensitive(true);
        Ok(Self { credential: value })
    }

    pub fn apply(&self, headers: &mut HeaderMap) {
        headers.insert(SESSION_CREDENTIAL_HEADER, self.credential.clone());
        headers.insert(CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    }
}

impl fmt::Debug for IssuedSessionHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IssuedSessionHeaders([REDACTED], no-store)")
    }
}

impl SessionCredential {
    fn encoded(&self) -> String {
        format!(
            "{}.{}",
            self.instance_id,
            self.resume_token.expose_encoded()
        )
    }

    /// Builds the sensitive HTTP Authorization header used by typed clients.
    ///
    /// # Errors
    /// Returns an invalid-value error if the credential cannot be encoded as an HTTP header.
    pub fn authorization_header(&self) -> Result<HeaderValue, InvalidValue> {
        let mut value = HeaderValue::from_str(&format!("{BEARER_PREFIX}{}", self.encoded()))
            .map_err(|_| invalid_credential())?;
        value.set_sensitive(true);
        Ok(value)
    }

    /// Applies the sensitive authorization header.
    ///
    /// # Errors
    /// Returns an invalid-value error if the credential cannot be encoded as an HTTP header.
    pub fn apply_authorization(&self, headers: &mut HeaderMap) -> Result<(), InvalidValue> {
        headers.insert(AUTHORIZATION, self.authorization_header()?);
        Ok(())
    }
    /// Parses the value following `Bearer` after enforcing a strict pre-dispatch bound.
    ///
    /// # Errors
    /// Returns an invalid-value error for malformed, oversized, or non-canonical credentials.
    pub fn parse_authorization(value: &str) -> Result<Self, InvalidValue> {
        if value.len() > BEARER_PREFIX.len() + MAX_CREDENTIAL_LENGTH {
            return Err(invalid_credential());
        }
        let encoded = value
            .strip_prefix(BEARER_PREFIX)
            .ok_or_else(invalid_credential)?;
        Self::parse_session_value(encoded)
    }

    /// Parses the adapter-only credential header value.
    ///
    /// # Errors
    /// Returns an invalid-value error unless there is exactly one separator and both parts are
    /// canonical, with the total bound checked before either part is parsed.
    pub fn parse_session_value(value: &str) -> Result<Self, InvalidValue> {
        if value.is_empty()
            || value.len() > MAX_CREDENTIAL_LENGTH
            || value.matches('.').count() != 1
        {
            return Err(invalid_credential());
        }
        let (instance, token) = value.split_once('.').ok_or_else(invalid_credential)?;
        Ok(Self {
            instance_id: InstanceId::new(instance).map_err(|_| invalid_credential())?,
            resume_token: ResumeToken::from_encoded(token).map_err(|_| invalid_credential())?,
        })
    }

    #[must_use]
    pub const fn instance_id(&self) -> &InstanceId {
        &self.instance_id
    }

    #[must_use]
    pub const fn resume_token(&self) -> &ResumeToken {
        &self.resume_token
    }
}

impl fmt::Debug for SessionCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionCredential([REDACTED])")
    }
}

const fn invalid_credential() -> InvalidValue {
    InvalidValue::new("authorization", "is not a valid session credential")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn credential_requires_bearer_and_exactly_one_dot() {
        let valid = format!("Bearer ins_one.{TOKEN}");
        let parsed = SessionCredential::parse_authorization(&valid).unwrap();
        assert_eq!(parsed.instance_id().as_str(), "ins_one");
        for invalid in [
            format!("ins_one.{TOKEN}"),
            "Bearer no-dot".into(),
            format!("Bearer ins_one..{TOKEN}"),
            format!("Bearer .{TOKEN}"),
        ] {
            assert!(SessionCredential::parse_authorization(&invalid).is_err());
        }
    }

    #[test]
    fn oversized_input_is_rejected_and_debug_is_redacted() {
        assert!(
            SessionCredential::parse_session_value(&"x".repeat(MAX_CREDENTIAL_LENGTH + 1)).is_err()
        );
        let credential =
            SessionCredential::parse_session_value(&format!("ins_one.{TOKEN}")).unwrap();
        let debug = format!("{credential:?}");
        assert_eq!(debug, "SessionCredential([REDACTED])");
        assert!(!debug.contains(TOKEN));
    }

    #[test]
    fn security_headers_are_an_explicit_contract() {
        assert_eq!(
            SENSITIVE_HEADERS,
            ["authorization", "psst-session-credential"]
        );
        assert_eq!(NO_STORE, "no-store");
        assert_eq!(
            header_value_for_log("Authorization", &format!("Bearer ins_one.{TOKEN}")),
            "[REDACTED]"
        );
        assert_eq!(
            header_value_for_log("Psst-Session-Credential", &format!("ins_one.{TOKEN}")),
            "[REDACTED]"
        );
        assert_eq!(
            header_value_for_log("content-type", "application/json"),
            "application/json"
        );
        let credential =
            SessionCredential::parse_session_value(&format!("ins_one.{TOKEN}")).unwrap();
        let mut request = HeaderMap::new();
        credential.apply_authorization(&mut request).unwrap();
        assert!(request[AUTHORIZATION].is_sensitive());
        let issued = IssuedSessionHeaders::new(&credential).unwrap();
        let mut response = HeaderMap::new();
        issued.apply(&mut response);
        assert!(response[SESSION_CREDENTIAL_HEADER].is_sensitive());
        assert_eq!(response[CACHE_CONTROL], NO_STORE);
        assert!(!format!("{issued:?}").contains(TOKEN));
    }
}
