use crate::{CredentialState, EffectiveConfigView, EffectiveValue, ValueSource};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use url::Url;

pub const DEFAULT_RELAY_ORIGIN: &str = "http://127.0.0.1:7341";

#[derive(Clone, Debug, Default)]
pub struct ConfigFlags {
    pub relay_origin: Option<String>,
    pub profile: Option<String>,
    pub config_path: Option<PathBuf>,
    pub relay_bind: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub allow_lan: Option<String>,
    pub log_level: Option<String>,
    pub log_format: Option<String>,
    pub max_message_bytes: Option<String>,
    pub max_long_poll_seconds: Option<String>,
    pub heartbeat_seconds: Option<String>,
    pub lease_seconds: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ConfigInputs {
    pub flags: ConfigFlags,
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl PlatformPaths {
    /// Resolves platform-native configuration, data, and runtime roots.
    ///
    /// # Errors
    /// Returns an error when the operating-system user directories are unavailable.
    pub fn detect() -> Result<Self, ConfigError> {
        #[cfg(not(target_os = "windows"))]
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or(ConfigError::PlatformPaths)?;
        #[cfg(target_os = "windows")]
        let (config, data, runtime) = {
            let roaming = env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or(ConfigError::PlatformPaths)?;
            let local = env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or(ConfigError::PlatformPaths)?;
            (
                roaming.join("psst"),
                local.join("psst"),
                local.join("psst").join("runtime"),
            )
        };
        #[cfg(target_os = "macos")]
        let (config, data, runtime) = {
            let p = home.join("Library/Application Support/psst");
            (p.clone(), p.clone(), p.join("runtime"))
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let (config, data, runtime) = {
            let config = env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("psst");
            let data = env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("psst");
            let runtime = env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| data.clone())
                .join("psst");
            (config, data, runtime)
        };
        Ok(Self {
            config_dir: config,
            data_dir: data,
            runtime_dir: runtime,
        })
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read,
    Parse,
    Invalid(&'static str),
    PlatformPaths,
}
impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Read => "configuration read failed",
            Self::Parse => "configuration parse failed",
            Self::Invalid(_) => "configuration value is invalid",
            Self::PlatformPaths => "platform paths unavailable",
        })
    }
}
impl std::error::Error for ConfigError {}

#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    pub relay_origin: EffectiveValue<String>,
    pub profile: EffectiveValue<String>,
    pub config_path: EffectiveValue<String>,
    pub relay_bind: EffectiveValue<String>,
    pub relay_data_dir: EffectiveValue<String>,
    pub allow_lan: EffectiveValue<bool>,
    pub log_level: EffectiveValue<String>,
    pub log_format: EffectiveValue<String>,
    pub max_message_bytes: EffectiveValue<u64>,
    pub max_long_poll_seconds: EffectiveValue<u32>,
    pub heartbeat_interval_seconds: EffectiveValue<u32>,
    pub lease_seconds: EffectiveValue<u32>,
    pub paths: PlatformPaths,
}
impl ResolvedConfig {
    #[must_use]
    pub fn view(&self, credential_state: CredentialState) -> EffectiveConfigView {
        EffectiveConfigView {
            relay_origin: self.relay_origin.clone(),
            profile: self.profile.clone(),
            config_path: self.config_path.clone(),
            relay_bind: self.relay_bind.clone(),
            relay_data_dir: self.relay_data_dir.clone(),
            allow_lan: self.allow_lan.clone(),
            log_level: self.log_level.clone(),
            log_format: self.log_format.clone(),
            max_message_bytes: self.max_message_bytes.clone(),
            max_long_poll_seconds: self.max_long_poll_seconds.clone(),
            heartbeat_interval_seconds: self.heartbeat_interval_seconds.clone(),
            lease_seconds: self.lease_seconds.clone(),
            credential_state,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    relay_origin: Option<String>,
    profile: Option<String>,
    relay_bind: Option<String>,
    data_dir: Option<PathBuf>,
    allow_lan: Option<bool>,
    log_level: Option<String>,
    log_format: Option<String>,
    max_message_bytes: Option<u64>,
    max_long_poll_seconds: Option<u32>,
    heartbeat_interval_seconds: Option<u32>,
    lease_seconds: Option<u32>,
}

pub struct ConfigResolver {
    paths: PlatformPaths,
}
impl ConfigResolver {
    #[must_use]
    pub const fn new(paths: PlatformPaths) -> Self {
        Self { paths }
    }
    /// Resolves and validates every field independently in frozen precedence order.
    ///
    /// # Errors
    /// Fails closed when a selected file cannot be read or any winning value is invalid.
    #[allow(clippy::too_many_lines)]
    pub fn resolve(&self, input: &ConfigInputs) -> Result<ResolvedConfig, ConfigError> {
        let config_path = input
            .flags
            .config_path
            .clone()
            .unwrap_or_else(|| self.paths.config_dir.join("config.yaml"));
        let file = read_config(&config_path)?;
        let pick = |flag: Option<String>, env_key: &str, file: Option<String>, default: &str| {
            if let Some(v) = flag {
                EffectiveValue {
                    value: v,
                    source: ValueSource::CommandLine,
                }
            } else if let Some(v) = input.environment.get(env_key) {
                EffectiveValue {
                    value: v.clone(),
                    source: ValueSource::Environment,
                }
            } else if let Some(v) = file {
                EffectiveValue {
                    value: v,
                    source: ValueSource::ConfigFile,
                }
            } else {
                EffectiveValue {
                    value: default.into(),
                    source: ValueSource::Default,
                }
            }
        };
        let mut origin = pick(
            input.flags.relay_origin.clone(),
            "PSST_RELAY",
            file.relay_origin,
            DEFAULT_RELAY_ORIGIN,
        );
        origin.value = canonical_relay_origin(&origin.value)?;
        let profile = pick(
            input.flags.profile.clone(),
            "PSST_PROFILE",
            file.profile,
            "default",
        );
        validate_profile_name(&profile.value)?;
        let bind = pick(
            input.flags.relay_bind.clone(),
            "PSST_RELAY_BIND",
            file.relay_bind,
            "127.0.0.1:7341",
        );
        bind.value
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::Invalid("relay_bind"))?;
        let data = pick(
            input
                .flags
                .data_dir
                .as_ref()
                .map(|p| p.display().to_string()),
            "PSST_DATA_DIR",
            file.data_dir.map(|p| p.display().to_string()),
            &self.paths.data_dir.display().to_string(),
        );
        if data.value.is_empty() {
            return Err(ConfigError::Invalid("data_dir"));
        }
        let allow = parsed(
            pick(
                input.flags.allow_lan.clone(),
                "PSST_ALLOW_LAN",
                file.allow_lan.map(|value| value.to_string()),
                "false",
            ),
            "allow_lan",
        )?;
        let level = pick(
            input.flags.log_level.clone(),
            "PSST_LOG",
            file.log_level,
            "info",
        );
        if !["trace", "debug", "info", "warn", "error"].contains(&level.value.as_str()) {
            return Err(ConfigError::Invalid("log_level"));
        }
        let format = pick(
            input.flags.log_format.clone(),
            "PSST_LOG_FORMAT",
            file.log_format,
            "text",
        );
        if !["text", "json"].contains(&format.value.as_str()) {
            return Err(ConfigError::Invalid("log_format"));
        }
        let message = bounded(
            parsed(
                pick(
                    input.flags.max_message_bytes.clone(),
                    "PSST_MAX_MESSAGE_BYTES",
                    file.max_message_bytes.map(|value| value.to_string()),
                    "65536",
                ),
                "max_message_bytes",
            )?,
            1,
            65_536,
            "max_message_bytes",
        )?;
        let poll = bounded(
            parsed(
                pick(
                    input.flags.max_long_poll_seconds.clone(),
                    "PSST_MAX_LONG_POLL_SECONDS",
                    file.max_long_poll_seconds.map(|value| value.to_string()),
                    "30",
                ),
                "max_long_poll_seconds",
            )?,
            0,
            30,
            "max_long_poll_seconds",
        )?;
        let heartbeat = bounded(
            parsed(
                pick(
                    input.flags.heartbeat_seconds.clone(),
                    "PSST_HEARTBEAT_SECONDS",
                    file.heartbeat_interval_seconds
                        .map(|value| value.to_string()),
                    "10",
                ),
                "heartbeat_interval_seconds",
            )?,
            1,
            300,
            "heartbeat_interval_seconds",
        )?;
        let lease = bounded(
            parsed(
                pick(
                    input.flags.lease_seconds.clone(),
                    "PSST_LEASE_SECONDS",
                    file.lease_seconds.map(|value| value.to_string()),
                    "30",
                ),
                "lease_seconds",
            )?,
            2,
            900,
            "lease_seconds",
        )?;
        if lease.value <= heartbeat.value {
            return Err(ConfigError::Invalid("lease_seconds"));
        }
        Ok(ResolvedConfig {
            relay_origin: origin,
            profile,
            config_path: EffectiveValue {
                value: config_path.display().to_string(),
                source: if input.flags.config_path.is_some() {
                    ValueSource::CommandLine
                } else {
                    ValueSource::Default
                },
            },
            relay_bind: bind,
            relay_data_dir: data,
            allow_lan: allow,
            log_level: level,
            log_format: format,
            max_message_bytes: message,
            max_long_poll_seconds: poll,
            heartbeat_interval_seconds: heartbeat,
            lease_seconds: lease,
            paths: self.paths.clone(),
        })
    }
}
fn read_config(path: &Path) -> Result<FileConfig, ConfigError> {
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let text = fs::read_to_string(path).map_err(|_| ConfigError::Read)?;
    serde_yaml::from_str(&text).map_err(|_| ConfigError::Parse)
}
#[allow(clippy::needless_pass_by_value)]
fn parsed<T: std::str::FromStr>(
    v: EffectiveValue<String>,
    name: &'static str,
) -> Result<EffectiveValue<T>, ConfigError> {
    Ok(EffectiveValue {
        value: v.value.parse().map_err(|_| ConfigError::Invalid(name))?,
        source: v.source,
    })
}
#[allow(clippy::needless_pass_by_value)]
fn bounded<T: PartialOrd>(
    v: EffectiveValue<T>,
    min: T,
    max: T,
    name: &'static str,
) -> Result<EffectiveValue<T>, ConfigError> {
    if v.value < min || v.value > max {
        Err(ConfigError::Invalid(name))
    } else {
        Ok(v)
    }
}
/// Canonicalizes an HTTP(S) origin without accepting paths or ambient authority.
///
/// # Errors
/// Rejects malformed URLs, user information, queries, fragments, and non-root paths.
pub fn canonical_relay_origin(raw: &str) -> Result<String, ConfigError> {
    let mut u = Url::parse(raw).map_err(|_| ConfigError::Invalid("relay_origin"))?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
    {
        return Err(ConfigError::Invalid("relay_origin"));
    }
    let default = if u.scheme() == "http" { 80 } else { 443 };
    if u.port() == Some(default) {
        u.set_port(None)
            .map_err(|()| ConfigError::Invalid("relay_origin"))?;
    }
    u.set_path("");
    Ok(u.to_string().trim_end_matches('/').to_owned())
}
/// Validates the bounded portable local profile-name grammar.
///
/// # Errors
/// Rejects empty, oversized, or non-portable names.
pub fn validate_profile_name(v: &str) -> Result<(), ConfigError> {
    if v.is_empty()
        || v.len() > 64
        || !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        Err(ConfigError::Invalid("profile"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paths(root: &Path) -> PlatformPaths {
        PlatformPaths {
            config_dir: root.join("c"),
            data_dir: root.join("d"),
            runtime_dir: root.join("r"),
        }
    }
    #[test]
    fn precedence_is_per_field_and_invalid_high_source_fails() {
        let t = tempfile::tempdir().unwrap();
        let p = paths(t.path());
        fs::create_dir_all(&p.config_dir).unwrap();
        fs::write(
            p.config_dir.join("config.yaml"),
            "relay_origin: http://file.example:80\nmax_message_bytes: 20\n",
        )
        .unwrap();
        let mut i = ConfigInputs::default();
        i.environment
            .insert("PSST_RELAY".into(), "https://ENV.example:443/".into());
        i.flags.max_message_bytes = Some("bad".into());
        assert!(ConfigResolver::new(p.clone()).resolve(&i).is_err());
        i.flags.max_message_bytes = Some("10".into());
        let got = ConfigResolver::new(p).resolve(&i).unwrap();
        assert_eq!(got.relay_origin.value, "https://env.example");
        assert_eq!(got.relay_origin.source, ValueSource::Environment);
        assert_eq!(got.max_message_bytes.value, 10);
        assert_eq!(got.max_message_bytes.source, ValueSource::CommandLine);
    }
    #[test]
    fn defaults_are_safe_and_bounds_are_closed() {
        let t = tempfile::tempdir().unwrap();
        let got = ConfigResolver::new(paths(t.path()))
            .resolve(&ConfigInputs::default())
            .unwrap();
        assert_eq!(got.relay_origin.value, DEFAULT_RELAY_ORIGIN);
        assert!(!got.allow_lan.value);
        assert_eq!(got.heartbeat_interval_seconds.value, 10);
        assert_eq!(got.lease_seconds.value, 30);
    }
    #[test]
    fn origin_contract_is_canonical_and_rejects_authority_extensions() {
        assert_eq!(
            canonical_relay_origin("HTTP://Example.COM:80/").unwrap(),
            "http://example.com"
        );
        for invalid in [
            "ftp://x",
            "http://u@x",
            "http://x/path",
            "http://x?q=1",
            "http://x/#f",
        ] {
            assert!(canonical_relay_origin(invalid).is_err());
        }
    }
}
