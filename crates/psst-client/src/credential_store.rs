use super::Credential;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialBinding {
    relay_origin: String,
    profile: String,
    squad_id: String,
    member_id: String,
}

impl CredentialBinding {
    /// Creates a canonical, portable credential authority binding.
    ///
    /// # Errors
    /// Rejects non-origin URLs, invalid profile names, and empty durable identifiers.
    pub fn new(
        relay_origin: &str,
        profile: &str,
        squad_id: &str,
        member_id: &str,
    ) -> io::Result<Self> {
        let relay_origin = canonical_origin(relay_origin)?;
        if profile.is_empty()
            || profile.len() > 64
            || !profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || squad_id.is_empty()
            || member_id.is_empty()
        {
            return Err(invalid());
        }
        Ok(Self {
            relay_origin,
            profile: profile.into(),
            squad_id: squad_id.into(),
            member_id: member_id.into(),
        })
    }
    #[must_use]
    pub fn relay_origin(&self) -> &str {
        &self.relay_origin
    }
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }
    #[must_use]
    pub fn squad_id(&self) -> &str {
        &self.squad_id
    }
    #[must_use]
    pub fn member_id(&self) -> &str {
        &self.member_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialFault {
    None,
    BeforeWrite,
    BeforeFlush,
    BeforePermission,
    BeforeReplace,
    #[cfg(test)]
    CrashAfterFlush,
}

pub struct CredentialStore {
    path: PathBuf,
    directory_guard: fs::File,
}
impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialStore")
            .field("state", &"restricted")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    relay_origin: String,
    profile: String,
    squad_id: String,
    member_id: String,
    instance_id: String,
    authorization: String,
}

impl CredentialStore {
    /// Opens one profile-owned store and removes only its recognized crash remnants.
    ///
    /// # Errors
    /// Rejects unsafe paths or remnants that cannot be securely removed.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(invalid)?;
        fs::create_dir_all(parent)?;
        reject_substitution(parent)?;
        let directory_guard = open_directory_guard(parent)?;
        let store = Self {
            path,
            directory_guard,
        };
        store.recover_temps()?;
        Ok(store)
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[must_use]
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }
    /// Atomically persists authority under its complete identity binding.
    ///
    /// # Errors
    /// Fails closed on unsafe paths, permissions, serialization, or durable-write failure.
    pub fn store(&self, binding: &CredentialBinding, credential: &Credential) -> io::Result<()> {
        self.store_with_fault(binding, credential, CredentialFault::None)
    }
    /// Persists authority with a deterministic test-only storage fault boundary.
    ///
    /// # Errors
    /// Returns an I/O error at the requested fault boundary or on storage failure.
    pub fn store_with_fault(
        &self,
        binding: &CredentialBinding,
        credential: &Credential,
        fault: CredentialFault,
    ) -> io::Result<()> {
        reject_substitution(&self.path)?;
        let authorization = credential.value.to_str().map_err(|_| invalid())?.to_owned();
        let record = Record {
            version: VERSION,
            relay_origin: binding.relay_origin.clone(),
            profile: binding.profile.clone(),
            squad_id: binding.squad_id.clone(),
            member_id: binding.member_id.clone(),
            instance_id: credential.instance_id.clone(),
            authorization,
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| invalid())?;
        #[cfg(unix)]
        return self.store_bytes_unix(&bytes, fault);
        #[cfg(windows)]
        {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)?;
                reject_substitution(parent)?;
            }
            if fault == CredentialFault::BeforeWrite {
                return Err(injected());
            }
            let parent = self.path.parent().ok_or_else(invalid)?;
            let (mut temp, temp_path) = create_owned_windows_temp(parent, &self.temp_prefix()?)?;
            verify_restricted_handle_acl(&temp)?;
            verify_restricted_handle(&temp)?;
            temp.write_all(&bytes)?;
            if fault == CredentialFault::BeforeFlush {
                return Err(injected());
            }
            temp.sync_all()?;
            #[cfg(test)]
            if fault == CredentialFault::CrashAfterFlush {
                std::process::abort();
            }
            if fault == CredentialFault::BeforePermission {
                return Err(injected());
            }
            verify_restricted_handle(&temp)?;
            if fault == CredentialFault::BeforeReplace {
                return Err(injected());
            }
            psst_platform_security::replace_file_by_handle(
                &temp,
                &self.directory_guard,
                self.path.file_name().ok_or_else(invalid)?,
            )?;
            drop(temp);
            verify_restricted(&self.path)?;
            debug_assert!(!temp_path.exists());
            Ok(())
        }
    }
    /// Reconstructs authority only when the restricted record matches the expected binding.
    ///
    /// # Errors
    /// Rejects corruption, unsafe permissions, path substitution, and stale binding.
    pub fn load(&self, expected: &CredentialBinding) -> io::Result<Credential> {
        #[cfg(windows)]
        reject_substitution(&self.path)?;
        #[cfg(windows)]
        let mut file = open_existing_secure(&self.path)?;
        #[cfg(unix)]
        let mut file = fs::File::from(
            rustix::fs::openat(
                &self.directory_guard,
                self.path.file_name().ok_or_else(invalid)?,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
        verify_restricted_handle(&file)?;
        #[cfg(windows)]
        verify_restricted(&self.path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if record.version != VERSION
            || record.relay_origin != expected.relay_origin
            || record.profile != expected.profile
            || record.squad_id != expected.squad_id
            || record.member_id != expected.member_id
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "credential binding mismatch",
            ));
        }
        let value =
            reqwest::header::HeaderValue::from_str(&record.authorization).map_err(|_| invalid())?;
        let raw = value
            .to_str()
            .map_err(|_| invalid())?
            .strip_prefix("Bearer ")
            .ok_or_else(invalid)?;
        let credential = Credential::from_response(
            &reqwest::header::HeaderValue::from_str(raw).map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?;
        if credential.instance_id != record.instance_id {
            return Err(invalid());
        }
        Ok(credential)
    }

    fn temp_prefix(&self) -> io::Result<String> {
        let name = self
            .path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(invalid)?;
        Ok(format!(".{name}.credential-tmp-"))
    }
    #[cfg(unix)]
    fn store_bytes_unix(&self, bytes: &[u8], fault: CredentialFault) -> io::Result<()> {
        use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
        let target = self.path.file_name().ok_or_else(invalid)?;
        let temp = self.temp_prefix()?;
        let _ = unlinkat(&self.directory_guard, temp.as_str(), AtFlags::empty());
        if fault == CredentialFault::BeforeWrite {
            return Err(injected());
        }
        let fd = openat(
            &self.directory_guard,
            temp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        let mut file = fs::File::from(fd);
        verify_restricted_handle(&file)?;
        file.write_all(bytes)?;
        if fault == CredentialFault::BeforeFlush {
            return Err(injected());
        }
        file.sync_all()?;
        #[cfg(test)]
        if fault == CredentialFault::CrashAfterFlush {
            std::process::abort();
        }
        if fault == CredentialFault::BeforePermission {
            return Err(injected());
        }
        verify_restricted_handle(&file)?;
        if fault == CredentialFault::BeforeReplace {
            return Err(injected());
        }
        renameat(
            &self.directory_guard,
            temp.as_str(),
            &self.directory_guard,
            target,
        )
        .map_err(io::Error::from)?;
        self.directory_guard.sync_all()?;
        Ok(())
    }
    fn recover_temps(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, unlinkat};
            let temp = self.temp_prefix()?;
            match unlinkat(&self.directory_guard, temp.as_str(), AtFlags::empty()) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::NOENT => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            let Some(parent) = self.path.parent() else {
                return Err(invalid());
            };
            if !parent.exists() {
                return Ok(());
            }
            reject_substitution(parent)?;
            let prefix = self.temp_prefix()?;
            for entry in fs::read_dir(parent)? {
                let entry = entry?;
                let name = entry.file_name();
                if name.to_str().is_some_and(|v| v.starts_with(&prefix)) {
                    let path = entry.path();
                    reject_substitution(&path)?;
                    let candidate = open_existing_secure(&path)?;
                    if !candidate.metadata()?.is_file() {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "unsafe credential remnant",
                        ));
                    }
                    verify_restricted_handle(&candidate)?;
                    verify_restricted(&path)?;
                    drop(candidate);
                    fs::remove_file(path)?;
                }
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
fn create_owned_windows_temp(parent: &Path, prefix: &str) -> io::Result<(fs::File, PathBuf)> {
    use std::fmt::Write as _;
    for _ in 0..32 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| io::Error::other("secure randomness unavailable"))?;
        let suffix = random
            .iter()
            .fold(String::with_capacity(32), |mut value, byte| {
                write!(value, "{byte:02x}").expect("writing to a String cannot fail");
                value
            });
        let path = parent.join(format!("{prefix}{suffix}"));
        let sid = current_process_sid()?;
        match psst_platform_security::create_restricted_file(&path, &sid) {
            Ok(file) => {
                return Ok((file, path));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "credential temporary namespace exhausted",
    ))
}

fn open_directory_guard(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    return psst_platform_security::open_pinned_directory(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut options = fs::OpenOptions::new();
        options.read(true);
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        options.open(path)
    }
}

fn canonical_origin(raw: &str) -> io::Result<String> {
    let mut value = url::Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(value.scheme(), "http" | "https")
        || value.host().is_none()
        || !value.username().is_empty()
        || value.password().is_some()
        || value.query().is_some()
        || value.fragment().is_some()
        || value.path() != "/"
    {
        return Err(invalid());
    }
    let default = if value.scheme() == "http" { 80 } else { 443 };
    if value.port() == Some(default) {
        value.set_port(None).map_err(|()| invalid())?;
    }
    value.set_path("");
    Ok(value.to_string().trim_end_matches('/').to_owned())
}

#[cfg(windows)]
fn open_existing_secure(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1 | 2).custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "reparse substitution rejected",
            ));
        }
    }
    Ok(file)
}
fn reject_substitution(path: &Path) -> io::Result<()> {
    let mut current = Some(path);
    while let Some(p) = current {
        match fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "link substitution rejected",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        #[cfg(windows)]
        if p.exists() {
            use std::os::windows::fs::MetadataExt;
            if fs::metadata(p)?.file_attributes() & 0x400 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "reparse substitution rejected",
                ));
            }
        }
        current = p.parent();
    }
    Ok(())
}
#[cfg(unix)]
fn verify_restricted_handle(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if file.metadata()?.permissions().mode() % 0o1000 == 0o600 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential permissions are unsafe",
        ))
    }
}
#[cfg(all(windows, test))]
fn restrict_handle(file: &fs::File) -> io::Result<()> {
    let sid = current_process_sid()?;
    psst_platform_security::restrict_file_to_sid(file, &sid)
}

#[cfg(windows)]
fn verify_restricted_handle_acl(file: &fs::File) -> io::Result<()> {
    psst_platform_security::verify_restricted_file(file, &current_process_sid()?)
}

#[cfg(windows)]
fn current_process_sid() -> io::Result<String> {
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "process identity unavailable",
        ));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.starts_with("S-1-") {
        Ok(value)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "process identity invalid",
        ))
    }
}
#[cfg(windows)]
fn verify_restricted(path: &Path) -> io::Result<()> {
    let sid = current_process_sid()?;
    let snapshot = tempfile::NamedTempFile::new()?;
    let status = std::process::Command::new("icacls")
        .arg(path)
        .arg("/save")
        .arg(snapshot.path())
        .args(["/c", "/q"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    let bytes = fs::read(snapshot.path())?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|v| u16::from_le_bytes([v[0], v[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let aces: Vec<_> = text
        .split('(')
        .skip(1)
        .filter_map(|part| part.split_once(')').map(|v| v.0))
        .collect();
    let safe = status.success()
        && text.contains("D:P")
        && !aces.is_empty()
        && aces.iter().all(|ace| {
            let fields: Vec<_> = ace.split(';').collect();
            fields.len() >= 6
                && fields[0] == "A"
                && fields[2].contains("FA")
                && fields[5].trim().eq_ignore_ascii_case(&sid)
        });
    if safe {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential ACL is unsafe",
        ))
    }
}
#[cfg(windows)]
fn verify_restricted_handle(file: &fs::File) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    if file.metadata()?.file_attributes() & 0x400 == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reparse substitution rejected",
        ))
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "credential record is invalid")
}
fn injected() -> io::Error {
    io::Error::other("injected credential-store failure")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> CredentialBinding {
        CredentialBinding::new("http://127.0.0.1:7341", "default", "squad_one", "mem_one").unwrap()
    }
    fn credential(value: &str) -> Credential {
        Credential::from_response(&reqwest::header::HeaderValue::from_str(value).unwrap()).unwrap()
    }
    #[test]
    fn restricted_record_round_trips_and_debug_is_redacted() {
        let t = tempfile::tempdir().unwrap();
        let store = CredentialStore::open(t.path().join("credential.json")).unwrap();
        let c = credential("ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        store.store(&binding(), &c).unwrap();
        let loaded = store.load(&binding()).unwrap();
        assert_eq!(format!("{loaded:?}"), "Credential([REDACTED])");
        assert!(!format!("{store:?}").contains("AAAA"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o077,
                0
            )
        }
    }
    #[test]
    fn binding_mismatch_corruption_and_faults_fail_closed_without_losing_prior() {
        let t = tempfile::tempdir().unwrap();
        let store = CredentialStore::open(t.path().join("credential.json")).unwrap();
        let first = credential("ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let next = credential("ins_two.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        store.store(&binding(), &first).unwrap();
        for fault in [
            CredentialFault::BeforeWrite,
            CredentialFault::BeforeFlush,
            CredentialFault::BeforePermission,
            CredentialFault::BeforeReplace,
        ] {
            assert!(store.store_with_fault(&binding(), &next, fault).is_err());
            assert_eq!(store.load(&binding()).unwrap().instance_id, "ins_one");
        }
        let wrong =
            CredentialBinding::new("http://127.0.0.1:7341", "default", "squad_one", "mem_other")
                .unwrap();
        assert!(store.load(&wrong).is_err());
        fs::write(store.path(), b"{}").unwrap();
        assert!(store.load(&binding()).is_err());
    }
    #[test]
    fn canary_exists_only_in_restricted_record_and_no_temp_survives() {
        let t = tempfile::tempdir().unwrap();
        let store = CredentialStore::open(t.path().join("credential.json")).unwrap();
        let canary = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        store
            .store(&binding(), &credential(&format!("ins_one.{canary}")))
            .unwrap();
        let mut positives = Vec::new();
        for e in fs::read_dir(t.path()).unwrap() {
            let p = e.unwrap().path();
            if fs::read(&p)
                .unwrap()
                .windows(canary.len())
                .any(|w| w == canary.as_bytes())
            {
                positives.push(p);
            }
        }
        assert_eq!(positives, [store.path().to_owned()]);
    }

    #[test]
    fn binding_constructor_rejects_ambient_authority_and_invalid_names() {
        for origin in ["http://u@host", "http://host/path", "ftp://host"] {
            assert!(CredentialBinding::new(origin, "default", "s", "m").is_err());
        }
        assert!(CredentialBinding::new("HTTP://EXAMPLE.COM:80/", "bad/name", "s", "m").is_err());
        assert_eq!(
            CredentialBinding::new("HTTP://EXAMPLE.COM:80/", "ok", "s", "m")
                .unwrap()
                .relay_origin(),
            "http://example.com"
        );
    }

    #[test]
    fn crash_writer_child() {
        let Ok(path) = std::env::var("PSST_TEST_CRASH_PATH") else {
            return;
        };
        let store = CredentialStore::open(PathBuf::from(path)).unwrap();
        store
            .store_with_fault(
                &binding(),
                &credential("ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
                CredentialFault::CrashAfterFlush,
            )
            .unwrap();
    }

    #[test]
    fn abrupt_crash_remnant_is_owned_and_scrubbed_before_open_returns() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("credential.json");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "credential_store::tests::crash_writer_child"])
            .env("PSST_TEST_CRASH_PATH", &path)
            .status()
            .unwrap();
        assert!(!status.success());
        let canary = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let remnant = fs::read_dir(t.path())
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(canary.len()).any(|value| value == canary))
                    .then_some(path)
            })
            .unwrap();
        #[cfg(windows)]
        verify_restricted(&remnant).unwrap();
        #[cfg(unix)]
        {
            let file = fs::File::from(
                rustix::fs::open(
                    &remnant,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .unwrap(),
            );
            verify_restricted_handle(&file).unwrap();
        }
        let _store = CredentialStore::open(path).unwrap();
        assert!(!fs::read_dir(t.path()).unwrap().any(|entry| {
            fs::read(entry.unwrap().path())
                .is_ok_and(|bytes| bytes.windows(canary.len()).any(|value| value == canary))
        }));
    }

    #[cfg(windows)]
    #[test]
    fn windows_acl_child() {
        let Ok(path) = std::env::var("PSST_TEST_ACL_PATH") else {
            return;
        };
        let store = CredentialStore::open(PathBuf::from(path)).unwrap();
        store
            .store(
                &binding(),
                &credential("ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            )
            .unwrap();
        store.load(&binding()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_acl_uses_token_sid_not_username_environment() {
        let t = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "credential_store::tests::windows_acl_child"])
            .env("PSST_TEST_ACL_PATH", t.path().join("credential.json"))
            .env("USERNAME", "attacker-controlled")
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(windows)]
    #[test]
    fn windows_extra_principal_is_rejected() {
        let t = tempfile::tempdir().unwrap();
        let store = CredentialStore::open(t.path().join("credential.json")).unwrap();
        store
            .store(
                &binding(),
                &credential("ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            )
            .unwrap();
        let status = std::process::Command::new("icacls")
            .arg(store.path())
            .args(["/grant", "*S-1-1-0:(R)", "/c"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(store.load(&binding()).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn windows_temp_is_pinned_and_unsafe_remnant_fails_closed() {
        let t = tempfile::tempdir().unwrap();
        let ancestor = t.path().join("state");
        let parent = ancestor.join("credentials");
        fs::create_dir_all(&parent).unwrap();
        let target = parent.join("credential.json");
        let guard = CredentialStore::open(target.clone()).unwrap();
        assert!(fs::remove_dir(&parent).is_err());
        assert!(fs::remove_dir_all(&ancestor).is_err());
        assert!(ancestor.exists());
        assert!(fs::rename(&ancestor, t.path().join("moved")).is_err());
        drop(guard);
        let prefix = ".credential.json.credential-tmp-";
        let (file, path) = create_owned_windows_temp(&parent, prefix).unwrap();
        restrict_handle(&file).unwrap();
        assert!(fs::rename(&path, parent.join("moved")).is_err());
        assert!(fs::remove_file(&path).is_err());
        drop(file);
        fs::remove_file(&path).unwrap();

        let unsafe_remnant = parent.join(format!("{prefix}attacker"));
        fs::write(&unsafe_remnant, b"not trusted").unwrap();
        assert!(CredentialStore::open(target).is_err());
        assert!(unsafe_remnant.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_creation_overrides_broad_inheritance_and_replace_is_complete() {
        let t = tempfile::tempdir().unwrap();
        let sid = current_process_sid().unwrap();
        let status = std::process::Command::new("icacls")
            .arg(t.path())
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("*{sid}:(OI)(CI)(F)"))
            .arg("*S-1-1-0:(OI)(CI)(F)")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let path = t.path().join("credential.json");
        let store = CredentialStore::open(path.clone()).unwrap();
        let first = "ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let second = "ins_two.BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA";
        store.store(&binding(), &credential(first)).unwrap();
        verify_restricted(&path).unwrap();
        store.store(&binding(), &credential(second)).unwrap();
        verify_restricted(&path).unwrap();
        let loaded = store.load(&binding()).unwrap();
        assert_eq!(loaded.instance_id, "ins_two");
        assert_eq!(loaded.value.to_str().unwrap(), format!("Bearer {second}"));
        assert!(
            !fs::read(&path)
                .unwrap()
                .windows(first.len())
                .any(|v| v == first.as_bytes())
        );
        assert!(!fs::read_dir(t.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("credential-tmp")
        }));
    }
    #[cfg(unix)]
    #[test]
    fn unix_symlink_and_unsafe_mode_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let t = tempfile::tempdir().unwrap();
        let store = CredentialStore::open(t.path().join("credential.json")).unwrap();
        store
            .store(
                &binding(),
                &credential("ins_one.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            )
            .unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load(&binding()).is_err());
        let link = t.path().join("link.json");
        symlink(store.path(), &link).unwrap();
        let linked = CredentialStore::open(link).unwrap();
        assert!(linked.load(&binding()).is_err());
    }
}
