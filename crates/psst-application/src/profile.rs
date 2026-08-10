use crate::{ConfigError, PlatformPaths, canonical_relay_origin, validate_profile_name};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, UdpSocket},
    path::{Path, PathBuf},
};

const PROFILE_VERSION: u32 = 1;
pub const MAX_PROFILE_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBinding {
    version: u32,
    pub profile: String,
    pub relay_origin: String,
    pub squad_name: String,
    pub squad_id: String,
    pub member_id: String,
}

impl ProfileBinding {
    /// Creates validated non-secret durable membership metadata.
    ///
    /// # Errors
    /// Rejects invalid names, origins, or empty identity bindings.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(
        profile: String,
        relay_origin: String,
        squad_name: String,
        squad_id: String,
        member_id: String,
    ) -> Result<Self, ConfigError> {
        let relay_origin = canonical_relay_origin(&relay_origin)?;
        let value = Self {
            version: PROFILE_VERSION,
            profile,
            relay_origin,
            squad_name,
            squad_id,
            member_id,
        };
        value.validate()?;
        Ok(value)
    }

    /// Validates a binding regardless of whether it came from construction or storage.
    ///
    /// # Errors
    /// Rejects unsupported versions, non-canonical origins, and invalid typed identities.
    pub fn validate(&self) -> Result<(), ConfigError> {
        use psst_core::{MembershipId, SquadId, SquadName};
        if self.version != PROFILE_VERSION {
            return Err(ConfigError::Invalid("profile binding version"));
        }
        validate_profile_name(&self.profile)?;
        if canonical_relay_origin(&self.relay_origin)? != self.relay_origin
            || SquadName::new(&self.squad_name).is_err()
            || SquadId::new(&self.squad_id).is_err()
            || MembershipId::new(&self.member_id).is_err()
        {
            return Err(ConfigError::Invalid("profile binding"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ProfilePaths {
    pub metadata: PathBuf,
    pub credential: PathBuf,
    pub lock: PathBuf,
}
impl ProfilePaths {
    /// Derives collision-resistant, platform-rooted paths for a canonical profile identity.
    ///
    /// # Errors
    /// Rejects invalid profile names and relay origins.
    pub fn for_profile(
        paths: &PlatformPaths,
        origin: &str,
        profile: &str,
    ) -> Result<Self, ConfigError> {
        validate_profile_name(profile)?;
        let origin = canonical_relay_origin(origin)?;
        let key = format!("{}-{}", profile, &hex_digest(origin.as_bytes())[..16]);
        Ok(Self {
            metadata: paths.data_dir.join("profiles").join(format!("{key}.json")),
            credential: paths
                .data_dir
                .join("credentials")
                .join(format!("{key}.json")),
            lock: paths.runtime_dir.join("locks").join(format!("{key}.lock")),
        })
    }

    /// Derives the non-secret harness-status record next to the canonical profile metadata.
    ///
    /// # Errors
    /// Returns an error only when the already-validated metadata path has no file stem.
    pub fn harness_status(&self) -> Result<PathBuf, ConfigError> {
        let stem = self
            .metadata
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or(ConfigError::Invalid("profile status path"))?;
        Ok(self
            .metadata
            .with_file_name(format!("{stem}.harness-v1.json")))
    }
}

pub struct ProfileLock {
    file: File,
    _directory_guard: File,
    _ownership_socket: TcpListener,
    _ownership_datagram: UdpSocket,
    path: PathBuf,
}
impl ProfileLock {
    /// Acquires exclusive ownership and retains it for this value's lifetime.
    ///
    /// # Errors
    /// Rejects substituted paths, inaccessible directories, and lock contention.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        reject_symlink(path)?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "lock parent unavailable")
        })?;
        fs::create_dir_all(parent)?;
        reject_symlink(parent)?;
        let canonical_parent = fs::canonicalize(parent)?;
        let identity =
            canonical_parent.join(path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "lock name unavailable")
            })?);
        let (stream_endpoint, datagram_endpoint) = lock_endpoints(&identity);
        let ownership_socket = TcpListener::bind(stream_endpoint)?;
        let ownership_datagram = UdpSocket::bind(datagram_endpoint)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        #[cfg(windows)]
        {
            options.share_mode(1 | 2).custom_flags(0x0020_0000);
        }
        let file = options.open(path)?;
        #[cfg(windows)]
        reject_handle_reparse(&file)?;
        file.try_lock_exclusive()?;
        Ok(Self {
            file,
            _directory_guard: open_directory_guard(parent)?,
            _ownership_socket: ownership_socket,
            _ownership_datagram: ownership_datagram,
            path: path.to_owned(),
        })
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
fn lock_endpoints(path: &Path) -> (SocketAddrV4, SocketAddrV4) {
    let digest = Sha256::digest(path.as_os_str().to_string_lossy().as_bytes());
    #[cfg(windows)]
    {
        // Windows does not consistently permit binding every address in 127/8.
        // Keep this OS-owned lock namespace on canonical loopback and below the
        // dynamic/private port range used by Windows.
        let stream_port = 10_000 + (u16::from_be_bytes([digest[0], digest[1]]) % 30_000);
        let datagram_port = 10_000 + (u16::from_be_bytes([digest[2], digest[3]]) % 30_000);
        (
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, stream_port),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, datagram_port),
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Linux starts its default ephemeral range at 32768. Keep ports below
        // that boundary and use the standardized 127/8 loopback network to
        // avoid mapping unrelated profile identities into only 30,000 slots.
        let stream_address = Ipv4Addr::new(127, digest[0], digest[1], digest[2]);
        let datagram_address = Ipv4Addr::new(127, digest[3], digest[4], digest[5]);
        let stream_port = 10_000 + (u16::from_be_bytes([digest[6], digest[7]]) % 20_000);
        let datagram_port = 10_000 + (u16::from_be_bytes([digest[8], digest[9]]) % 20_000);
        (
            SocketAddrV4::new(stream_address, stream_port),
            SocketAddrV4::new(datagram_address, datagram_port),
        )
    }
    #[cfg(target_os = "macos")]
    {
        // macOS only permits binding canonical 127.0.0.1 on supported runners.
        // Its ephemeral range starts at 49152, so this fixed-address namespace
        // remains below unrelated outbound socket allocation.
        let stream_port = 10_000 + (u16::from_be_bytes([digest[0], digest[1]]) % 30_000);
        let datagram_port = 10_000 + (u16::from_be_bytes([digest[2], digest[3]]) % 30_000);
        (
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, stream_port),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, datagram_port),
        )
    }
}
impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Loads and validates non-secret profile metadata.
///
/// # Errors
/// Rejects corruption, unsupported versions, non-canonical origins, and path substitution.
pub fn load_profile(path: &Path) -> io::Result<Option<ProfileBinding>> {
    if !path.exists() {
        return Ok(None);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "profile parent unavailable"))?;
    let directory_guard = open_directory_guard(parent)?;
    #[cfg(windows)]
    let _ = &directory_guard;
    reject_symlink(path)?;
    #[cfg(windows)]
    let mut file = OpenOptions::new();
    #[cfg(windows)]
    file.read(true);
    #[cfg(windows)]
    {
        file.share_mode(1 | 2).custom_flags(0x0020_0000);
    }
    #[cfg(windows)]
    let file = file.open(path)?;
    #[cfg(unix)]
    let file = File::from(
        rustix::fs::openat(
            &directory_guard,
            path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "profile name unavailable")
            })?,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    #[cfg(windows)]
    reject_handle_reparse(&file)?;
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_PROFILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid profile record size",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PROFILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "profile record too large",
        ));
    }
    let value: ProfileBinding = serde_json::from_slice(&bytes).map_err(invalid_data)?;
    value.validate().map_err(invalid_data)?;
    Ok(Some(value))
}

/// Removes non-secret profile metadata after confirmed membership leave.
///
/// # Errors
/// Rejects substituted paths and propagates removal failures.
pub fn clear_profile(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    reject_symlink(path)?;
    #[cfg(unix)]
    {
        use rustix::fs::{AtFlags, unlinkat};
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "profile parent unavailable")
        })?;
        let directory = open_directory_guard(parent)?;
        unlinkat(
            &directory,
            path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "profile name unavailable")
            })?,
            AtFlags::empty(),
        )
        .map_err(io::Error::from)?;
        directory.sync_all()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let mut options = OpenOptions::new();
        options
            .access_mode(0x8000_0000 | 0x0001_0000)
            .share_mode(1 | 2)
            .custom_flags(0x0020_0000);
        let file = options.open(path)?;
        reject_handle_reparse(&file)?;
        psst_platform_security::delete_held_file(&file)
    }
}

/// Verifies that a runtime relay override cannot retarget a bound profile.
///
/// # Errors
/// Returns an error when the requested origin is invalid or differs after canonicalization.
pub fn verify_profile_origin(binding: &ProfileBinding, requested: &str) -> Result<(), ConfigError> {
    if canonical_relay_origin(requested)? == binding.relay_origin {
        Ok(())
    } else {
        Err(ConfigError::Invalid("profile origin mismatch"))
    }
}

/// Atomically stores non-secret profile metadata.
///
/// # Errors
/// Rejects path substitution and propagates durable-write failures.
pub fn store_profile(path: &Path, binding: &ProfileBinding) -> io::Result<()> {
    binding.validate().map_err(invalid_data)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "profile parent unavailable"))?;
    fs::create_dir_all(parent)?;
    reject_symlink(parent)?;
    let directory_guard = open_directory_guard(parent)?;
    store_profile_guarded(path, binding, &directory_guard)
}
fn store_profile_guarded(
    path: &Path,
    binding: &ProfileBinding,
    directory_guard: &File,
) -> io::Result<()> {
    let _ = directory_guard;
    let bytes = serde_json::to_vec(binding).map_err(invalid_data)?;
    atomic_replace(path, &bytes)
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        atomic_replace_unix(path, bytes)
    }
    #[cfg(windows)]
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            reject_symlink(parent)?;
        }
        reject_symlink(path)?;
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid record path"))?;
        let temp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
        reject_symlink(&temp)?;
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temp, path)?;
            if let Some(parent) = path.parent() {
                File::open(parent)
                    .and_then(|d| d.sync_all())
                    .or_else(|e| if cfg!(windows) { Ok(()) } else { Err(e) })?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(unix)]
fn atomic_replace_unix(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, open, openat, renameat, unlinkat};
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "profile parent unavailable"))?;
    fs::create_dir_all(parent)?;
    let directory = File::from(
        open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let target = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "profile name unavailable"))?;
    let temp = format!(".{}.tmp", target.to_string_lossy());
    let _ = unlinkat(&directory, temp.as_str(), AtFlags::empty());
    let fd = openat(
        &directory,
        temp.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(io::Error::from)?;
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    file.sync_all()?;
    renameat(&directory, temp.as_str(), &directory, target).map_err(io::Error::from)?;
    directory.sync_all()
}

pub(crate) fn reject_symlink(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "link substitution rejected",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    };
    result?;
    #[cfg(windows)]
    if path.exists() {
        use std::os::windows::fs::MetadataExt;
        if fs::metadata(path)?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "reparse substitution rejected",
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn reject_handle_reparse(file: &File) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    if file.metadata()?.file_attributes() & 0x400 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reparse substitution rejected",
        ));
    }
    Ok(())
}
pub(crate) fn open_directory_guard(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        options
            .share_mode(1 | 2)
            .custom_flags(0x0200_0000 | 0x0020_0000);
    }
    options.open(path)
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        })
}
fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_is_exclusive_for_process_lifetime() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x.lock");
        let first = ProfileLock::acquire(&p).unwrap();
        assert!(ProfileLock::acquire(&p).is_err());
        drop(first);
        assert!(ProfileLock::acquire(&p).is_ok());
    }
    #[test]
    fn metadata_round_trips_without_secret_material() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("profile.json");
        let b = ProfileBinding::new(
            "alpha".into(),
            "HTTP://EXAMPLE.COM:80/".into(),
            "alpha".into(),
            "sqd_1".into(),
            "mem_1".into(),
        )
        .unwrap();
        store_profile(&p, &b).unwrap();
        assert_eq!(load_profile(&p).unwrap(), Some(b));
        let text = fs::read_to_string(p).unwrap();
        assert!(!text.to_lowercase().contains("credential"));
        assert!(!text.to_lowercase().contains("token"));
    }
    #[test]
    fn metadata_rejects_zero_oversize_and_invalid_domain_bindings() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("profile.json");
        fs::write(&path, []).unwrap();
        assert!(load_profile(&path).is_err());
        fs::write(
            &path,
            vec![b'x'; usize::try_from(MAX_PROFILE_BYTES).unwrap() + 1],
        )
        .unwrap();
        assert!(load_profile(&path).is_err());
        assert!(
            ProfileBinding::new(
                "alpha".into(),
                "http://127.0.0.1:7341".into(),
                "Not-Canonical".into(),
                "sqd_ok".into(),
                "mem_ok".into(),
            )
            .is_err()
        );
    }
    #[test]
    fn pathname_replacement_cannot_create_a_second_owner() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("locks").join("x.lock");
        let guard = ProfileLock::acquire(&p).unwrap();
        let _ = fs::rename(&p, t.path().join("replacement.lock"));
        let _ = fs::rename(p.parent().unwrap(), t.path().join("replacement-dir"));
        assert!(ProfileLock::acquire(&p).is_err());
        drop(guard);
        assert!(ProfileLock::acquire(&p).is_ok());
    }
    #[test]
    fn unrelated_occupied_kernel_endpoint_fails_closed() {
        let t = tempfile::tempdir().unwrap();
        let parent = t.path().join("locks");
        fs::create_dir_all(&parent).unwrap();
        let p = parent.join("x.lock");
        let identity = fs::canonicalize(&parent).unwrap().join("x.lock");
        let (tcp, udp) = lock_endpoints(&identity);
        let stream = TcpListener::bind(tcp).unwrap();
        assert!(ProfileLock::acquire(&p).is_err());
        drop(stream);
        let datagram = UdpSocket::bind(udp).unwrap();
        assert!(ProfileLock::acquire(&p).is_err());
        drop(datagram);
        // The deliberately bounded macOS/Windows kernel namespace can also be
        // occupied conservatively by another profile-lock test running in this
        // process. Once our injected occupant is gone, require eventual bounded
        // acquisition rather than assuming the shared namespace is immediately
        // idle at this exact scheduler tick.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            match ProfileLock::acquire(&p) {
                Ok(guard) => {
                    drop(guard);
                    break;
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("profile lock remained unavailable: {error}"),
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_lock_endpoints_use_full_loopback_identity_below_ephemeral_ports() {
        let first = lock_endpoints(Path::new("/tmp/psst-profile-a.lock"));
        let second = lock_endpoints(Path::new("/tmp/psst-profile-b.lock"));
        assert_ne!(first, second);
        for endpoint in [first.0, first.1, second.0, second.1] {
            assert_eq!(endpoint.ip().octets()[0], 127);
            assert!((10_000..30_000).contains(&endpoint.port()));
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lock_endpoints_use_bindable_loopback_below_ephemeral_ports() {
        let (stream, datagram) = lock_endpoints(Path::new("/tmp/psst-profile.lock"));
        for endpoint in [stream, datagram] {
            assert_eq!(*endpoint.ip(), Ipv4Addr::LOCALHOST);
            assert!((10_000..40_000).contains(&endpoint.port()));
        }
    }
    #[cfg(unix)]
    #[test]
    fn metadata_and_lock_reject_symbolic_links() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let target = t.path().join("target");
        fs::write(&target, b"{}").unwrap();
        let link = t.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(load_profile(&link).is_err());
        assert!(ProfileLock::acquire(&link).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn metadata_delete_handle_blocks_replacement_until_exact_object_is_removed() {
        use std::os::windows::fs::OpenOptionsExt;
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("profile.json");
        let replacement = t.path().join("replacement.json");
        fs::write(&path, b"original").unwrap();
        fs::write(&replacement, b"replacement").unwrap();
        let mut options = OpenOptions::new();
        options
            .access_mode(0x8000_0000 | 0x0001_0000)
            .share_mode(1 | 2)
            .custom_flags(0x0020_0000);
        let held = options.open(&path).unwrap();
        reject_handle_reparse(&held).unwrap();
        assert!(fs::rename(&replacement, &path).is_err());
        assert!(fs::remove_file(&path).is_err());
        psst_platform_security::delete_held_file(&held).unwrap();
        drop(held);
        assert!(!path.exists());
        assert_eq!(fs::read(&replacement).unwrap(), b"replacement");
    }
}
