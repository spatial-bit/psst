use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use fs2::FileExt;
use reqwest::{Client, Url, redirect};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const DEFAULT_CHANNEL_URL: &str =
    "https://github.com/spatial-bit/psst/releases/download/dogfood-channel/windows-x86_64.json";
const CHANNEL_SCHEMA: &str = "psst.install-channel.v1";
const INSTALL_RECORD_SCHEMA: &str = "psst.install-record.v1";
const WINDOWS_TARGET: &str = "windows-x86_64";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_BINARY_BYTES: usize = 128 * 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64;
const MAX_URL_BYTES: usize = 2_048;
pub const UPDATE_KEY_ID: &str = "671bbe9c89459c0003ca72a3cfb5f3cc9b56adfeb1aa1372c062406bba6df6c3";
const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0x28, 0xf8, 0x60, 0xd8, 0xe6, 0x03, 0x2a, 0xab, 0x08, 0xb3, 0x9b, 0xe5, 0xc2, 0xde, 0x12, 0xfd,
    0xd9, 0x98, 0xf2, 0x47, 0x0b, 0xa2, 0x60, 0xb4, 0xae, 0xbb, 0x92, 0x3a, 0x5b, 0x4d, 0x5f, 0x18,
];

#[derive(Debug)]
pub enum SetupError {
    InvalidArguments(&'static str),
    UnsupportedPlatform,
    InvalidChannel(&'static str),
    Network(String),
    Io(io::Error),
    Busy,
    IntegrityMismatch,
    InstalledBinaryFailed,
    PathUpdate(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArguments(message) | Self::InvalidChannel(message) => {
                formatter.write_str(message)
            }
            Self::UnsupportedPlatform => {
                formatter.write_str("This installer build supports 64-bit Windows only.")
            }
            Self::Network(message) => write!(formatter, "Download failed: {message}"),
            Self::Io(error) => write!(formatter, "Installation failed: {error}"),
            Self::Busy => formatter.write_str(
                "Psst is currently running or another setup is active. Stop it and try again.",
            ),
            Self::IntegrityMismatch => formatter.write_str(
                "The downloaded Psst executable did not match the published size and SHA-256.",
            ),
            Self::InstalledBinaryFailed => formatter.write_str(
                "The new Psst executable did not start correctly. The prior version was restored.",
            ),
            Self::PathUpdate(message) => write!(formatter, "PATH update failed: {message}"),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<io::Error> for SetupError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChannelManifest {
    pub schema: String,
    pub version: String,
    pub revision: String,
    pub target: String,
    pub key_id: String,
    pub publication_run: String,
    pub psst_url: String,
    pub psst_bytes: u64,
    pub psst_sha256: String,
    pub signature: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct InstallRecord {
    schema: String,
    version: String,
    revision: String,
    target: String,
    psst_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    Updated,
    AlreadyCurrent,
}

#[derive(Clone, Debug)]
pub struct InstallResult {
    pub outcome: InstallOutcome,
    pub install_dir: PathBuf,
    pub version: String,
    pub revision: String,
    pub path_changed: bool,
    pub path_warning: Option<String>,
}

/// Return the standard per-user Windows installation directory.
///
/// # Errors
///
/// Returns an error outside Windows or when the account's local application-data directory is not
/// available.
pub fn default_install_dir() -> Result<PathBuf, SetupError> {
    if !cfg!(windows) {
        return Err(SetupError::UnsupportedPlatform);
    }
    let local = std::env::var_os("LOCALAPPDATA").ok_or(SetupError::InvalidArguments(
        "LOCALAPPDATA is unavailable for this Windows account.",
    ))?;
    Ok(PathBuf::from(local).join("Programs").join("Psst"))
}

/// Download and validate the bounded update-channel document.
///
/// # Errors
///
/// Returns an error for an unapproved origin, transport failure, oversized response, malformed
/// JSON, or any closed channel-contract violation.
pub async fn fetch_channel(url: &str) -> Result<ChannelManifest, SetupError> {
    let channel_url = validate_url(url, true)?;
    let client = http_client()?;
    let response = client
        .get(channel_url)
        .send()
        .await
        .map_err(network_error)?;
    let bytes = read_bounded(response, MAX_MANIFEST_BYTES).await?;
    let manifest: ChannelManifest = serde_json::from_slice(&bytes).map_err(|_| {
        SetupError::InvalidChannel("The update channel returned malformed metadata.")
    })?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Download the executable named by a validated channel document.
///
/// # Errors
///
/// Returns an error for an invalid channel, unapproved origin, transport failure, oversized
/// response, size/hash mismatch, or non-PE payload.
pub async fn fetch_binary(manifest: &ChannelManifest) -> Result<Vec<u8>, SetupError> {
    validate_manifest(manifest)?;
    let url = validate_url(&manifest.psst_url, false)?;
    let response = http_client()?
        .get(url)
        .send()
        .await
        .map_err(network_error)?;
    let limit = usize::try_from(manifest.psst_bytes)
        .map_err(|_| SetupError::InvalidChannel("The published executable is too large."))?;
    let bytes = read_bounded(response, limit.min(MAX_BINARY_BYTES)).await?;
    if bytes.len() as u64 != manifest.psst_bytes
        || sha256(&bytes) != manifest.psst_sha256
        || !bytes.starts_with(b"MZ")
    {
        return Err(SetupError::IntegrityMismatch);
    }
    Ok(bytes)
}

/// Validate the closed Windows channel contract without issuing network or filesystem operations.
///
/// # Errors
///
/// Returns an error when any field is missing, out of bounds, malformed, or names an unapproved
/// target or download origin.
pub fn validate_manifest(manifest: &ChannelManifest) -> Result<(), SetupError> {
    let key = VerifyingKey::from_bytes(&UPDATE_PUBLIC_KEY)
        .map_err(|_| SetupError::InvalidChannel("The embedded update key is invalid."))?;
    validate_manifest_with_key(manifest, &key)
}

fn validate_manifest_with_key(
    manifest: &ChannelManifest,
    key: &VerifyingKey,
) -> Result<(), SetupError> {
    if manifest.schema != CHANNEL_SCHEMA {
        return Err(SetupError::InvalidChannel(
            "The update channel schema is unsupported.",
        ));
    }
    if manifest.target != WINDOWS_TARGET {
        return Err(SetupError::InvalidChannel(
            "The update channel target is not Windows x86-64.",
        ));
    }
    if manifest.version.is_empty()
        || manifest.version.len() > MAX_VERSION_BYTES
        || !manifest
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(SetupError::InvalidChannel(
            "The published version is invalid.",
        ));
    }
    if manifest.revision.len() != 40
        || !manifest
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(SetupError::InvalidChannel(
            "The published revision is invalid.",
        ));
    }
    if manifest.key_id != UPDATE_KEY_ID {
        return Err(SetupError::InvalidChannel(
            "The update signing key identity is not trusted.",
        ));
    }
    validate_publication_run(&manifest.publication_run)?;
    if manifest.psst_bytes < 2 || manifest.psst_bytes > MAX_BINARY_BYTES as u64 {
        return Err(SetupError::InvalidChannel(
            "The published executable size is invalid.",
        ));
    }
    if manifest.psst_sha256.len() != 64
        || !manifest
            .psst_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(SetupError::InvalidChannel(
            "The published SHA-256 is invalid.",
        ));
    }
    validate_url(&manifest.psst_url, false)?;
    if manifest.signature.len() != 88 {
        return Err(SetupError::InvalidChannel(
            "The update signature is malformed.",
        ));
    }
    let signature_bytes = BASE64
        .decode(&manifest.signature)
        .map_err(|_| SetupError::InvalidChannel("The update signature is malformed."))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| SetupError::InvalidChannel("The update signature is malformed."))?;
    key.verify(&canonical_payload(manifest), &signature)
        .map_err(|_| SetupError::InvalidChannel("The update signature is invalid."))?;
    Ok(())
}

#[must_use]
pub fn canonical_payload(manifest: &ChannelManifest) -> Vec<u8> {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        manifest.schema,
        manifest.version,
        manifest.revision,
        manifest.target,
        manifest.key_id,
        manifest.publication_run,
        manifest.psst_url,
        manifest.psst_bytes,
        manifest.psst_sha256
    )
    .into_bytes()
}

/// Atomically install already downloaded and verified Psst bytes for one Windows account.
///
/// # Errors
///
/// Returns an error before replacement on integrity or lock failure. If the new executable fails
/// its smoke check after replacement, the function restores the prior executable before returning.
pub fn install_verified(
    manifest: &ChannelManifest,
    binary: &[u8],
    install_dir: &Path,
    update_path: bool,
) -> Result<InstallResult, SetupError> {
    install_verified_with_validation(
        manifest,
        binary,
        install_dir,
        update_path,
        validate_manifest,
        smoke_binary,
    )
}

fn install_verified_with_validation(
    manifest: &ChannelManifest,
    binary: &[u8],
    install_dir: &Path,
    update_path: bool,
    validate: impl FnOnce(&ChannelManifest) -> Result<(), SetupError>,
    smoke: impl FnOnce(&Path, &str) -> bool,
) -> Result<InstallResult, SetupError> {
    validate(manifest)?;
    if binary.len() as u64 != manifest.psst_bytes
        || sha256(binary) != manifest.psst_sha256
        || !binary.starts_with(b"MZ")
    {
        return Err(SetupError::IntegrityMismatch);
    }
    fs::create_dir_all(install_dir)?;
    let lock_path = install_dir.join("setup.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock_exclusive().map_err(|_| SetupError::Busy)?;

    let current = install_dir.join("psst.exe");
    let previous = install_dir.join("psst.previous.exe");
    let staged = install_dir.join(format!("psst.new-{}.exe", manifest.revision));
    if current.is_file() && hash_file(&current)? == manifest.psst_sha256 {
        let (path_changed, path_warning) = update_path_with_warning(install_dir, update_path);
        return Ok(result(
            InstallOutcome::AlreadyCurrent,
            install_dir,
            manifest,
            path_changed,
            path_warning,
        ));
    }
    write_new_file(&staged, binary)?;
    let had_current = current.is_file();
    if previous.exists() {
        fs::remove_file(&previous).map_err(map_busy)?;
    }
    if had_current {
        fs::rename(&current, &previous).map_err(map_busy)?;
    }
    if let Err(error) = fs::rename(&staged, &current) {
        if had_current {
            let _ = fs::rename(&previous, &current);
        }
        return Err(map_busy(error));
    }
    if !smoke(&current, &manifest.version) {
        let _ = fs::remove_file(&current);
        if had_current {
            let _ = fs::rename(&previous, &current);
        }
        return Err(SetupError::InstalledBinaryFailed);
    }
    if let Err(error) = write_record(install_dir, manifest) {
        restore_prior_binary(&current, &previous, had_current);
        return Err(error);
    }
    let (path_changed, path_warning) = update_path_with_warning(install_dir, update_path);
    Ok(result(
        if had_current {
            InstallOutcome::Updated
        } else {
            InstallOutcome::Installed
        },
        install_dir,
        manifest,
        path_changed,
        path_warning,
    ))
}

fn result(
    outcome: InstallOutcome,
    install_dir: &Path,
    manifest: &ChannelManifest,
    path_changed: bool,
    path_warning: Option<String>,
) -> InstallResult {
    InstallResult {
        outcome,
        install_dir: install_dir.to_path_buf(),
        version: manifest.version.clone(),
        revision: manifest.revision.clone(),
        path_changed,
        path_warning,
    }
}

fn restore_prior_binary(current: &Path, previous: &Path, had_current: bool) {
    let _ = fs::remove_file(current);
    if had_current {
        let _ = fs::rename(previous, current);
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), SetupError> {
    if path.exists() {
        fs::remove_file(path).map_err(map_busy)?;
    }
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    Ok(())
}

fn write_record(dir: &Path, manifest: &ChannelManifest) -> Result<(), SetupError> {
    let record = InstallRecord {
        schema: INSTALL_RECORD_SCHEMA.to_owned(),
        version: manifest.version.clone(),
        revision: manifest.revision.clone(),
        target: manifest.target.clone(),
        psst_sha256: manifest.psst_sha256.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|_| SetupError::InvalidChannel("Could not serialize the install record."))?;
    let staged = dir.join("installed.new.json");
    let current = dir.join("installed.json");
    write_new_file(&staged, &bytes)?;
    if current.exists() {
        fs::remove_file(&current)?;
    }
    fs::rename(staged, current)?;
    Ok(())
}

fn smoke_binary(path: &Path, version: &str) -> bool {
    Command::new(path)
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && output.stderr.is_empty()
                && String::from_utf8(output.stdout)
                    .is_ok_and(|text| text.trim() == format!("psst {version}"))
        })
}

fn hash_file(path: &Path) -> Result<String, SetupError> {
    if fs::metadata(path)?.len() > MAX_BINARY_BYTES as u64 {
        return Err(SetupError::IntegrityMismatch);
    }
    let mut input = File::open(path)?;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut digest = Sha256::new();
    let mut total = 0_usize;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read);
        if total > MAX_BINARY_BYTES {
            return Err(SetupError::IntegrityMismatch);
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_url(value: &str, channel: bool) -> Result<Url, SetupError> {
    if value.len() > MAX_URL_BYTES {
        return Err(SetupError::InvalidChannel("The published URL is too long."));
    }
    let url = Url::parse(value)
        .map_err(|_| SetupError::InvalidChannel("The published URL is invalid."))?;
    if url.scheme() != "https" || !is_allowed_host(url.host_str()) {
        return Err(SetupError::InvalidChannel(if channel {
            "The update channel must use an approved GitHub HTTPS origin."
        } else {
            "The executable must use an approved GitHub HTTPS origin."
        }));
    }
    Ok(url)
}

fn is_allowed_host(host: Option<&str>) -> bool {
    matches!(
        host,
        Some(
            "github.com"
                | "api.github.com"
                | "raw.githubusercontent.com"
                | "objects.githubusercontent.com"
                | "release-assets.githubusercontent.com"
        )
    )
}

fn validate_publication_run(value: &str) -> Result<(), SetupError> {
    let url = validate_url(value, false)?;
    if url.host_str() != Some("github.com") {
        return Err(SetupError::InvalidChannel(
            "The publication run is not a GitHub Actions URL.",
        ));
    }
    let prefix = "/spatial-bit/psst/actions/runs/";
    let run = url
        .path()
        .strip_prefix(prefix)
        .ok_or(SetupError::InvalidChannel(
            "The publication run is not for spatial-bit/psst.",
        ))?;
    if run.is_empty() || !run.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SetupError::InvalidChannel(
            "The publication run identifier is invalid.",
        ));
    }
    Ok(())
}

fn http_client() -> Result<Client, SetupError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 4 || !is_allowed_host(attempt.url().host_str()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("psst-setup/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(network_error)
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, SetupError> {
    if !response.status().is_success() {
        return Err(SetupError::Network(format!(
            "the server returned HTTP {}",
            response.status().as_u16()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(SetupError::Network(
            "the response exceeded its size limit".to_owned(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(SetupError::Network(
                "the response exceeded its size limit".to_owned(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn network_error(error: impl std::fmt::Display) -> SetupError {
    SetupError::Network(error.to_string())
}

fn map_busy(error: io::Error) -> SetupError {
    if matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) {
        SetupError::Busy
    } else {
        SetupError::Io(error)
    }
}

fn update_user_path_if_requested(dir: &Path, requested: bool) -> Result<bool, SetupError> {
    if !requested {
        return Ok(false);
    }
    update_user_path(dir)
}

fn update_path_with_warning(dir: &Path, requested: bool) -> (bool, Option<String>) {
    match update_user_path_if_requested(dir, requested) {
        Ok(changed) => (changed, None),
        Err(error) => (false, Some(error.to_string())),
    }
}

#[cfg(windows)]
fn update_user_path(dir: &Path) -> Result<bool, SetupError> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, REG_SZ};
    use winreg::{RegKey, RegValue};

    let environment = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .map_err(|error| SetupError::PathUpdate(error.to_string()))?;
    let current: String = environment.get_value("Path").unwrap_or_default();
    let Some(updated) = append_windows_path(&current, &dir.to_string_lossy()) else {
        return Ok(false);
    };
    let value_type = environment
        .get_raw_value("Path")
        .map_or(REG_EXPAND_SZ, |value| {
            if matches!(value.vtype, REG_SZ | REG_EXPAND_SZ) {
                value.vtype
            } else {
                REG_EXPAND_SZ
            }
        });
    let words: Vec<u16> = updated.encode_utf16().chain(std::iter::once(0)).collect();
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    environment
        .set_raw_value(
            "Path",
            &RegValue {
                bytes,
                vtype: value_type,
            },
        )
        .map_err(|error| SetupError::PathUpdate(error.to_string()))?;
    broadcast_environment_change();
    Ok(true)
}

#[cfg(windows)]
fn append_windows_path(current: &str, wanted: &str) -> Option<String> {
    if current
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| part.eq_ignore_ascii_case(wanted))
    {
        return None;
    }
    if current.trim_matches(';').is_empty() {
        Some(wanted.to_owned())
    } else {
        Some(format!("{};{}", current.trim_end_matches(';'), wanted))
    }
}

#[cfg(windows)]
fn broadcast_environment_change() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let environment: Vec<u16> = OsStr::new("Environment")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut ignored = 0;
    // SAFETY: the UTF-16 buffer is NUL-terminated and remains live for the synchronous call. The
    // broadcast carries no process-owned pointer beyond this bounded timeout.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2_000,
            &raw mut ignored,
        );
    }
}

#[cfg(not(windows))]
fn update_user_path(_dir: &Path) -> Result<bool, SetupError> {
    Err(SetupError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn manifest(binary: &[u8]) -> ChannelManifest {
        let mut manifest = ChannelManifest {
            schema: CHANNEL_SCHEMA.to_owned(),
            version: "0.1.0-alpha.2".to_owned(),
            revision: "1".repeat(40),
            target: WINDOWS_TARGET.to_owned(),
            key_id: UPDATE_KEY_ID.to_owned(),
            publication_run: "https://github.com/spatial-bit/psst/actions/runs/123".to_owned(),
            psst_url: "https://github.com/spatial-bit/psst/releases/download/test/psst.exe"
                .to_owned(),
            psst_bytes: binary.len() as u64,
            psst_sha256: sha256(binary),
            signature: String::new(),
        };
        resign(&mut manifest);
        manifest
    }

    fn resign(manifest: &mut ChannelManifest) {
        manifest.signature =
            BASE64.encode(test_key().sign(&canonical_payload(manifest)).to_bytes());
    }

    fn validate_test_manifest(manifest: &ChannelManifest) -> Result<(), SetupError> {
        validate_manifest_with_key(manifest, &test_key().verifying_key())
    }

    #[test]
    fn manifest_is_closed_bounded_and_github_only() {
        let valid = manifest(b"MZ-valid");
        validate_test_manifest(&valid).unwrap();
        let mut invalid = valid.clone();
        invalid.target = "windows-arm64".to_owned();
        assert!(validate_test_manifest(&invalid).is_err());
        invalid = valid.clone();
        invalid.psst_url = "https://example.com/psst.exe".to_owned();
        assert!(validate_test_manifest(&invalid).is_err());
        invalid = valid.clone();
        invalid.revision = "A".repeat(40);
        assert!(validate_test_manifest(&invalid).is_err());
        invalid = valid.clone();
        invalid.version = "0.1.0-alpha.3".to_owned();
        assert!(matches!(
            validate_test_manifest(&invalid),
            Err(SetupError::InvalidChannel(
                "The update signature is invalid."
            ))
        ));
    }

    #[test]
    fn integrity_rejects_size_hash_and_non_pe_content_before_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let binary = b"MZ-valid";
        let valid = manifest(binary);
        assert!(matches!(
            install_verified_with_validation(
                &valid,
                b"MZ-wrong",
                temp.path(),
                false,
                validate_test_manifest,
                |_, _| true,
            ),
            Err(SetupError::IntegrityMismatch)
        ));
        let not_pe = b"not-an-exe";
        let invalid = manifest(not_pe);
        assert!(matches!(
            install_verified_with_validation(
                &invalid,
                not_pe,
                temp.path(),
                false,
                validate_test_manifest,
                |_, _| true,
            ),
            Err(SetupError::IntegrityMismatch)
        ));
        assert!(!temp.path().join("psst.exe").exists());
    }

    #[test]
    fn locked_installer_is_fail_fast() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(temp.path().join("setup.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let binary = b"MZ-valid";
        assert!(matches!(
            install_verified_with_validation(
                &manifest(binary),
                binary,
                temp.path(),
                false,
                validate_test_manifest,
                |_, _| true,
            ),
            Err(SetupError::Busy)
        ));
    }

    #[test]
    fn unknown_manifest_fields_are_rejected() {
        let mut json = serde_json::to_value(manifest(b"MZ-valid")).unwrap();
        json.as_object_mut().unwrap().insert(
            "credential".to_owned(),
            serde_json::Value::String("forbidden".to_owned()),
        );
        assert!(serde_json::from_value::<ChannelManifest>(json).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_append_is_idempotent_and_preserves_existing_expansions() {
        let current = r"%USERPROFILE%\bin;C:\Tools";
        assert_eq!(
            append_windows_path(current, r"C:\Psst").as_deref(),
            Some(r"%USERPROFILE%\bin;C:\Tools;C:\Psst")
        );
        assert_eq!(append_windows_path(current, r"c:\tools"), None);
        assert_eq!(
            append_windows_path("", r"C:\Psst").as_deref(),
            Some(r"C:\Psst")
        );
    }

    #[test]
    fn install_update_and_repeat_are_atomic_and_keep_one_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        let data = temp.path().join("data-must-survive");
        fs::write(&data, b"user-state").unwrap();
        let first_binary = b"MZ-first";
        let first = manifest(first_binary);
        let installed = install_verified_with_validation(
            &first,
            first_binary,
            &install,
            false,
            validate_test_manifest,
            |_, _| true,
        )
        .unwrap();
        assert_eq!(installed.outcome, InstallOutcome::Installed);
        assert_eq!(fs::read(install.join("psst.exe")).unwrap(), first_binary);
        assert_eq!(fs::read(&data).unwrap(), b"user-state");

        let second_binary = b"MZ-second";
        let mut second = manifest(second_binary);
        second.revision = "3".repeat(40);
        resign(&mut second);
        let updated = install_verified_with_validation(
            &second,
            second_binary,
            &install,
            false,
            validate_test_manifest,
            |_, _| true,
        )
        .unwrap();
        assert_eq!(updated.outcome, InstallOutcome::Updated);
        assert_eq!(fs::read(install.join("psst.exe")).unwrap(), second_binary);
        assert_eq!(
            fs::read(install.join("psst.previous.exe")).unwrap(),
            first_binary
        );

        let repeated = install_verified_with_validation(
            &second,
            second_binary,
            &install,
            false,
            validate_test_manifest,
            |_, _| panic!("an identical install must not relaunch the executable"),
        )
        .unwrap();
        assert_eq!(repeated.outcome, InstallOutcome::AlreadyCurrent);
        assert_eq!(fs::read(&data).unwrap(), b"user-state");
    }

    #[test]
    fn failed_new_binary_smoke_restores_the_prior_executable() {
        let temp = tempfile::tempdir().unwrap();
        let first_binary = b"MZ-first";
        let first = manifest(first_binary);
        install_verified_with_validation(
            &first,
            first_binary,
            temp.path(),
            false,
            validate_test_manifest,
            |_, _| true,
        )
        .unwrap();

        let second_binary = b"MZ-broken";
        let mut second = manifest(second_binary);
        second.revision = "4".repeat(40);
        resign(&mut second);
        assert!(matches!(
            install_verified_with_validation(
                &second,
                second_binary,
                temp.path(),
                false,
                validate_test_manifest,
                |_, _| false,
            ),
            Err(SetupError::InstalledBinaryFailed)
        ));
        assert_eq!(
            fs::read(temp.path().join("psst.exe")).unwrap(),
            first_binary
        );
    }
}
