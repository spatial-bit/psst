// The final profile directory is the application-owned trusted root. Ancestors are pinned only to
// prevent retargeting; on Unix the held final directory must be effective-UID-owned and not
// group/other writable. This does not claim protection from a malicious same-account process.

use crate::{ProfileBinding, canonical_relay_origin};
use psst_protocol::ApiTimestamp;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeavePhase {
    Intent,
    Confirmed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeaveJournal {
    version: u32,
    phase: LeavePhase,
    relay_origin: String,
    profile: String,
    squad_name: String,
    squad_id: String,
    member_id: String,
    operation_id: String,
    created_at: ApiTimestamp,
    confirmed_at: Option<ApiTimestamp>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum JournalFault {
    #[default]
    None,
    BeforeTempName,
    BeforeCreate,
    BeforeWrite,
    BeforeFileSync,
    BeforeReplace,
    BeforePostReplaceSync,
    #[cfg(all(windows, test))]
    CrashAfterPostReplaceSync,
    BeforeRemove,
    BeforeRemoveDirectorySync,
}

pub(crate) struct LeaveJournalStore {
    path: PathBuf,
    directory: File,
    #[cfg(unix)]
    _ancestor_guards: Vec<File>,
}

pub(crate) fn sibling_path(metadata: &Path) -> io::Result<PathBuf> {
    let parent = metadata.parent().ok_or_else(invalid_input)?;
    let stem = metadata.file_stem().ok_or_else(invalid_input)?;
    let mut name = stem.to_os_string();
    name.push(".leave-v1.json");
    Ok(parent.join(name))
}

impl LeaveJournal {
    pub(crate) fn intent(
        binding: &ProfileBinding,
        operation_id: String,
        created_at: ApiTimestamp,
    ) -> io::Result<Self> {
        let value = Self {
            version: VERSION,
            phase: LeavePhase::Intent,
            relay_origin: binding.relay_origin.clone(),
            profile: binding.profile.clone(),
            squad_name: binding.squad_name.clone(),
            squad_id: binding.squad_id.clone(),
            member_id: binding.member_id.clone(),
            operation_id,
            created_at,
            confirmed_at: None,
        };
        value.validate(binding)?;
        Ok(value)
    }

    pub(crate) fn confirmed(&self, confirmed_at: ApiTimestamp) -> io::Result<Self> {
        if self.phase != LeavePhase::Intent {
            return Err(invalid_data());
        }
        let mut value = self.clone();
        value.phase = LeavePhase::Confirmed;
        value.confirmed_at = Some(confirmed_at);
        Ok(value)
    }

    pub(crate) const fn phase(&self) -> LeavePhase {
        self.phase
    }

    pub(crate) fn validate(&self, binding: &ProfileBinding) -> io::Result<()> {
        binding.validate().map_err(|_| invalid_data())?;
        self.validate_record()?;
        if self.relay_origin != binding.relay_origin
            || self.profile != binding.profile
            || self.squad_name != binding.squad_name
            || self.squad_id != binding.squad_id
            || self.member_id != binding.member_id
        {
            return Err(invalid_data());
        }
        Ok(())
    }

    fn validate_record(&self) -> io::Result<()> {
        let binding = ProfileBinding::new(
            self.profile.clone(),
            self.relay_origin.clone(),
            self.squad_name.clone(),
            self.squad_id.clone(),
            self.member_id.clone(),
        )
        .map_err(|_| invalid_data())?;
        let operation_valid = (1..=128).contains(&self.operation_id.len())
            && self
                .operation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if self.version != VERSION
            || binding.relay_origin != self.relay_origin
            || !operation_valid
            || (self.phase == LeavePhase::Confirmed) != self.confirmed_at.is_some()
            || self
                .confirmed_at
                .is_some_and(|value| value < self.created_at)
        {
            return Err(invalid_data());
        }
        Ok(())
    }
}

impl LeaveJournalStore {
    pub(crate) fn open(metadata: &Path) -> io::Result<Self> {
        let path = sibling_path(metadata)?;
        let parent = path.parent().ok_or_else(invalid_input)?;
        if !parent.is_dir() {
            return Err(invalid_input());
        }
        reject_substitution(parent)?;
        #[cfg(unix)]
        let (directory, ancestor_guards) = open_unix_directory_chain(parent)?;
        #[cfg(unix)]
        verify_unix_profile_directory(&directory)?;
        #[cfg(windows)]
        let directory = open_directory(parent)?;
        let store = Self {
            path,
            directory,
            #[cfg(unix)]
            _ancestor_guards: ancestor_guards,
        };
        store.reject_target_substitution()?;
        Ok(store)
    }

    pub(crate) fn load(&self, binding: &ProfileBinding) -> io::Result<Option<LeaveJournal>> {
        let Some(value) = self.load_record()? else {
            return Ok(None);
        };
        value.validate(binding)?;
        Ok(Some(value))
    }

    pub(crate) fn load_for_profile_key(
        &self,
        relay_origin: &str,
        profile: &str,
    ) -> io::Result<Option<(LeaveJournal, ProfileBinding)>> {
        let canonical = canonical_relay_origin(relay_origin).map_err(|_| invalid_input())?;
        let Some(value) = self.load_record()? else {
            return Ok(None);
        };
        if value.relay_origin != canonical || value.profile != profile {
            return Err(invalid_data());
        }
        let binding = ProfileBinding::new(
            value.profile.clone(),
            value.relay_origin.clone(),
            value.squad_name.clone(),
            value.squad_id.clone(),
            value.member_id.clone(),
        )
        .map_err(|_| invalid_data())?;
        value.validate(&binding)?;
        Ok(Some((value, binding)))
    }

    fn load_record(&self) -> io::Result<Option<LeaveJournal>> {
        let Some(file) = self.open_target()? else {
            return Ok(None);
        };
        let length = file.metadata()?.len();
        if length == 0 || length > MAX_JOURNAL_BYTES {
            return Err(invalid_data());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(length).map_err(|_| invalid_data())?);
        file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_JOURNAL_BYTES {
            return Err(invalid_data());
        }
        let value: LeaveJournal = serde_json::from_slice(&bytes).map_err(|_| invalid_data())?;
        value.validate_record()?;
        Ok(Some(value))
    }

    pub(crate) fn store(&self, value: &LeaveJournal) -> io::Result<()> {
        self.store_with_fault(value, JournalFault::None)
    }

    pub(crate) fn store_with_fault(
        &self,
        value: &LeaveJournal,
        fault: JournalFault,
    ) -> io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| invalid_data())?;
        value.validate_record()?;
        if bytes.len() > usize::try_from(MAX_JOURNAL_BYTES).unwrap_or(usize::MAX) {
            return Err(invalid_data());
        }
        self.reject_target_substitution()?;
        #[cfg(unix)]
        return self.store_unix(&bytes, fault);
        #[cfg(windows)]
        return self.store_windows(&bytes, fault);
    }

    pub(crate) fn remove(&self) -> io::Result<()> {
        self.remove_with_fault(JournalFault::None)
    }

    pub(crate) fn remove_with_fault(&self, fault: JournalFault) -> io::Result<()> {
        if fault == JournalFault::BeforeRemove {
            return Err(injected());
        }
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, unlinkat};
            match unlinkat(&self.directory, self.target_name()?, AtFlags::empty()) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
                Err(error) => return Err(io::Error::from(error)),
            }
        }
        #[cfg(windows)]
        {
            let Some(file) = self.open_target_for_delete()? else {
                return Ok(());
            };
            psst_platform_security::delete_held_file(&file)?;
        }
        if fault == JournalFault::BeforeRemoveDirectorySync {
            return Err(injected());
        }
        sync_directory(&self.directory)
    }

    fn target_name(&self) -> io::Result<&std::ffi::OsStr> {
        self.path.file_name().ok_or_else(invalid_input)
    }

    fn temp_name(&self) -> io::Result<String> {
        let name = self.target_name()?.to_str().ok_or_else(invalid_input)?;
        let mut random = [0_u8; 16];
        fill_random(&mut random)?;
        let suffix = random
            .iter()
            .fold(String::with_capacity(32), |mut value, byte| {
                use std::fmt::Write as _;
                write!(value, "{byte:02x}").expect("writing to a String cannot fail");
                value
            });
        Ok(format!(".{name}.tmp-{suffix}"))
    }

    fn reject_target_substitution(&self) -> io::Result<()> {
        self.open_target().map(|_| ())
    }

    fn open_target(&self) -> io::Result<Option<File>> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            match openat(
                &self.directory,
                self.target_name()?,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => {
                    let file = File::from(fd);
                    if file.metadata()?.is_file() {
                        verify_journal_handle(&file)?;
                        Ok(Some(file))
                    } else {
                        Err(substitution())
                    }
                }
                Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
                Err(error) => Err(io::Error::from(error)),
            }
        }
        #[cfg(windows)]
        {
            match psst_platform_security::open_relative_file(
                &self.directory,
                self.target_name()?,
                false,
            )? {
                Some(file) if is_reparse(&file)? => Err(substitution()),
                Some(file) if file.metadata()?.is_file() => {
                    verify_journal_handle(&file)?;
                    Ok(Some(file))
                }
                Some(_) => Err(substitution()),
                None => Ok(None),
            }
        }
    }

    #[cfg(windows)]
    fn open_target_for_delete(&self) -> io::Result<Option<File>> {
        match psst_platform_security::open_relative_file(
            &self.directory,
            self.target_name()?,
            true,
        )? {
            Some(file) if is_reparse(&file)? => Err(substitution()),
            Some(file) if file.metadata()?.is_file() => {
                verify_journal_handle(&file)?;
                Ok(Some(file))
            }
            Some(_) => Err(substitution()),
            None => Ok(None),
        }
    }

    #[cfg(unix)]
    fn store_unix(&self, bytes: &[u8], fault: JournalFault) -> io::Result<()> {
        use rustix::fs::{Mode, OFlags, openat, renameat};
        if fault == JournalFault::BeforeTempName {
            return Err(injected());
        }
        let temp = self.temp_name()?;
        if fault == JournalFault::BeforeCreate {
            return Err(injected());
        }
        let fd = openat(
            &self.directory,
            temp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        let mut file = File::from(fd);
        if fault == JournalFault::BeforeWrite {
            return Err(injected());
        }
        file.write_all(bytes)?;
        if fault == JournalFault::BeforeFileSync {
            return Err(injected());
        }
        file.sync_all()?;
        if fault == JournalFault::BeforeReplace {
            return Err(injected());
        }
        renameat(
            &self.directory,
            temp.as_str(),
            &self.directory,
            self.target_name()?,
        )
        .map_err(io::Error::from)?;
        if fault == JournalFault::BeforePostReplaceSync {
            return Err(injected());
        }
        sync_directory(&self.directory)
    }

    #[cfg(windows)]
    fn store_windows(&self, bytes: &[u8], fault: JournalFault) -> io::Result<()> {
        if fault == JournalFault::BeforeTempName {
            return Err(injected());
        }
        let temp_name = self.temp_name()?;
        if fault == JournalFault::BeforeCreate {
            return Err(injected());
        }
        let sid = psst_platform_security::current_process_sid()?;
        psst_platform_security::verify_local_ntfs_path(&self.path)
            .map_err(|error| staged("volume_check", error))?;
        let mut file = psst_platform_security::create_relative_restricted_file(
            &self.directory,
            std::ffi::OsStr::new(&temp_name),
            &sid,
        )?;
        if is_reparse(&file)? {
            return Err(substitution());
        }
        psst_platform_security::verify_restricted_file(&file, &sid)?;
        if fault == JournalFault::BeforeWrite {
            return Err(injected());
        }
        file.write_all(bytes)?;
        if fault == JournalFault::BeforeFileSync {
            return Err(injected());
        }
        file.sync_all()
            .map_err(|error| staged("file_sync", error))?;
        if fault == JournalFault::BeforeReplace {
            return Err(injected());
        }
        psst_platform_security::replace_file_by_handle(&file, &self.directory, self.target_name()?)
            .map_err(|error| staged("replace", error))?;
        if fault == JournalFault::BeforePostReplaceSync {
            return Err(injected());
        }
        file.sync_all()
            .map_err(|error| staged("post_replace_file_sync", error))?;
        #[cfg(test)]
        if fault == JournalFault::CrashAfterPostReplaceSync {
            std::process::abort();
        }
        sync_directory(&self.directory)
    }
}

#[cfg(windows)]
fn open_directory(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::MetadataExt;
    let directory = psst_platform_security::open_pinned_directory(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err(substitution());
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_unix_directory_chain(path: &Path) -> io::Result<(File, Vec<File>)> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use std::path::Component;
    if !path.is_absolute() {
        return Err(invalid_input());
    }
    // macOS exposes ordinary temporary paths through aliases such as `/var` -> `/private/var`.
    // Resolve only the ancestors outside the application-owned profile directory, then open and
    // verify that final trusted directory without following it. The resulting descriptor chain is
    // still pinned and every component of the resolved path is opened with NOFOLLOW.
    let trusted_name = path.file_name().ok_or_else(invalid_input)?;
    let external_parent = path.parent().ok_or_else(invalid_input)?;
    let resolved_path = fs::canonicalize(external_parent)?.join(trusted_name);
    let mut current = File::from(
        open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let mut ancestors = Vec::new();
    for component in resolved_path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let next = File::from(
                    openat(
                        &current,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(io::Error::from)?,
                );
                ancestors.push(current);
                current = next;
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(invalid_input());
            }
        }
    }
    Ok((current, ancestors))
}

#[cfg(unix)]
fn verify_unix_profile_directory(directory: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = directory.metadata()?;
    if metadata.uid() != psst_platform_security::effective_uid() || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "application-owned profile directory is unsafe",
        ));
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)] // Windows removal is replay-safe; Unix must fsync the directory.
fn sync_directory(directory: &File) -> io::Result<()> {
    #[cfg(unix)]
    return directory.sync_all();
    #[cfg(windows)]
    {
        // Stores verify NTFS on the write-through file handle before replacement. Marker removal
        // is intentionally replay-safe, so a stale marker is reconciled again after a crash.
        let _ = directory;
        Ok(())
    }
}

fn reject_substitution(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(substitution()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn verify_journal_handle(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o777 == 0o600 {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "leave journal permissions are unsafe",
        ))
    }
    #[cfg(windows)]
    {
        psst_platform_security::verify_restricted_file(
            file,
            &psst_platform_security::current_process_sid()?,
        )
    }
}

#[cfg(windows)]
fn is_reparse(file: &File) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    Ok(file.metadata()?.file_attributes() & 0x400 != 0)
}

fn invalid_input() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid leave journal path")
}
fn invalid_data() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid leave journal")
}
fn substitution() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "leave journal path substitution rejected",
    )
}
fn injected() -> io::Error {
    io::Error::other("injected leave journal fault")
}
#[allow(clippy::needless_pass_by_value)]
#[cfg(windows)]
fn staged(stage: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("leave journal {stage} failed: {error}"),
    )
}

#[cfg(unix)]
fn fill_random(output: &mut [u8]) -> io::Result<()> {
    let mut source = File::open("/dev/urandom")?;
    source.read_exact(output)
}

#[cfg(windows)]
fn fill_random(output: &mut [u8]) -> io::Result<()> {
    psst_platform_security::fill_secure_random(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::process::Command as ChildProcess;

    fn timestamp(value: &str) -> ApiTimestamp {
        serde_json::from_str(&format!("\"{value}\"")).unwrap()
    }
    fn binding(root: &Path) -> (ProfileBinding, PathBuf) {
        fs::create_dir_all(root.join("profiles")).unwrap();
        let binding = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:7341".into(),
            "alpha".into(),
            "sqd_alpha".into(),
            "mem_worker".into(),
        )
        .unwrap();
        (binding, root.join("profiles/default.json"))
    }
    fn intent(binding: &ProfileBinding) -> LeaveJournal {
        LeaveJournal::intent(
            binding,
            "leave_0123456789abcdef".into(),
            timestamp("2026-08-08T01:02:03.004Z"),
        )
        .unwrap()
    }

    #[test]
    fn closed_canonical_record_round_trips_and_remove_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        let intent = intent(&binding);
        store.store(&intent).unwrap();
        assert_eq!(store.load(&binding).unwrap(), Some(intent.clone()));
        let confirmed = intent
            .confirmed(timestamp("2026-08-08T01:02:04.005Z"))
            .unwrap();
        store.store(&confirmed).unwrap();
        assert_eq!(store.load(&binding).unwrap(), Some(confirmed));
        store.remove().unwrap();
        store.remove().unwrap();
        assert_eq!(store.load(&binding).unwrap(), None);
    }

    #[test]
    fn metadata_keyed_siblings_isolate_profiles_in_one_directory() {
        let temp = tempfile::tempdir().unwrap();
        let (first_binding, first_metadata) = binding(temp.path());
        let second_binding = ProfileBinding::new(
            "second".into(),
            "http://127.0.0.1:7341".into(),
            "beta".into(),
            "sqd_beta".into(),
            "mem_second".into(),
        )
        .unwrap();
        let second_metadata = first_metadata.with_file_name("second.json");
        assert_ne!(
            sibling_path(&first_metadata).unwrap(),
            sibling_path(&second_metadata).unwrap()
        );
        let first = LeaveJournalStore::open(&first_metadata).unwrap();
        let second = LeaveJournalStore::open(&second_metadata).unwrap();
        first.store(&intent(&first_binding)).unwrap();
        second.store(&intent(&second_binding)).unwrap();
        assert!(first.load(&first_binding).unwrap().is_some());
        assert!(second.load(&second_binding).unwrap().is_some());
        first.remove().unwrap();
        assert!(first.load(&first_binding).unwrap().is_none());
        assert!(second.load(&second_binding).unwrap().is_some());
    }

    #[test]
    fn every_store_fault_preserves_old_or_complete_new_record() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        let old = intent(&binding);
        store.store(&old).unwrap();
        let new = old
            .confirmed(timestamp("2026-08-08T01:02:04.005Z"))
            .unwrap();
        for fault in [
            JournalFault::BeforeTempName,
            JournalFault::BeforeCreate,
            JournalFault::BeforeWrite,
            JournalFault::BeforeFileSync,
            JournalFault::BeforeReplace,
            JournalFault::BeforePostReplaceSync,
        ] {
            let _ = store.store(&old);
            assert!(store.store_with_fault(&new, fault).is_err());
            let observed = store.load(&binding).unwrap().unwrap();
            if fault == JournalFault::BeforePostReplaceSync {
                assert_eq!(observed, new);
            } else {
                assert_eq!(observed, old);
            }
        }
    }

    #[test]
    fn remove_faults_are_idempotently_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        let value = intent(&binding);
        store.store(&value).unwrap();
        assert!(store.remove_with_fault(JournalFault::BeforeRemove).is_err());
        assert_eq!(store.load(&binding).unwrap(), Some(value));
        assert!(
            store
                .remove_with_fault(JournalFault::BeforeRemoveDirectorySync)
                .is_err()
        );
        assert_eq!(store.load(&binding).unwrap(), None);
        store.remove().unwrap();
    }

    #[test]
    fn corruption_unknown_fields_binding_mismatch_and_secret_canary_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        let value = intent(&binding);
        store.store(&value).unwrap();
        let bytes = fs::read(&store.path).unwrap();
        assert!(!bytes.windows(11).any(|window| window == b"Bearer TEST"));
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["authorization"] = serde_json::json!("Bearer TEST_SECRET_CANARY");
        fs::write(&store.path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(store.load(&binding).is_err());
        fs::write(&store.path, b"{not-json").unwrap();
        assert!(store.load(&binding).is_err());
        store.store(&value).unwrap();
        let other = ProfileBinding::new(
            "other".into(),
            binding.relay_origin.clone(),
            binding.squad_name.clone(),
            binding.squad_id.clone(),
            binding.member_id.clone(),
        )
        .unwrap();
        assert!(store.load(&other).is_err());
    }

    #[test]
    fn recursive_secret_canary_occurs_only_in_the_separate_credential_fixture() {
        fn scan(root: &Path, needle: &[u8], found: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(root).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path, needle, found);
                } else if fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|part| part == needle))
                {
                    found.push(path);
                }
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        store.store(&intent(&binding)).unwrap();
        let credential = temp.path().join("credentials/restricted.json");
        fs::create_dir_all(credential.parent().unwrap()).unwrap();
        let canary = b"PSST_RAW_CREDENTIAL_CANARY_7f67d4";
        fs::write(&credential, canary).unwrap();
        let mut found = Vec::new();
        scan(temp.path(), canary, &mut found);
        assert_eq!(found, vec![credential]);
    }

    #[test]
    fn hostile_temp_candidate_is_never_deleted_or_reused() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        let hostile = store.path.with_file_name(".leave-journal.json.tmp-hostile");
        fs::write(&hostile, b"attacker-owned remnant").unwrap();
        store.store(&intent(&binding)).unwrap();
        assert_eq!(fs::read(&hostile).unwrap(), b"attacker-owned remnant");
        assert!(store.load(&binding).unwrap().is_some());
    }

    #[test]
    fn hostile_existing_target_fails_closed_before_replace() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        let old = intent(&binding);
        store.store(&old).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&store.path, fs::Permissions::from_mode(0o666)).unwrap();
        }
        #[cfg(windows)]
        {
            let status = ChildProcess::new("icacls.exe")
                .arg(&store.path)
                .args(["/grant", "*S-1-1-0:F", "/q"])
                .status()
                .unwrap();
            assert!(status.success());
        }
        let new = old
            .confirmed(timestamp("2026-08-08T01:02:04.005Z"))
            .unwrap();
        assert!(store.store(&new).is_err());
        assert_ne!(
            fs::read(&store.path).unwrap(),
            serde_json::to_vec(&new).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_symlink_alias_is_resolved_before_trusted_directory_is_pinned() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let attacker = temp.path().join("attacker");
        fs::create_dir_all(real.join("profiles")).unwrap();
        fs::create_dir_all(attacker.join("profiles")).unwrap();
        let alias = temp.path().join("alias");
        symlink(&real, &alias).unwrap();

        let binding = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:7341".into(),
            "alpha".into(),
            "sqd_alpha".into(),
            "mem_worker".into(),
        )
        .unwrap();
        let metadata = alias.join("profiles/default.json");
        let store = LeaveJournalStore::open(&metadata).unwrap();

        fs::remove_file(&alias).unwrap();
        symlink(&attacker, &alias).unwrap();
        store.store(&intent(&binding)).unwrap();

        assert!(
            sibling_path(&real.join("profiles/default.json"))
                .unwrap()
                .is_file()
        );
        assert!(
            !sibling_path(&attacker.join("profiles/default.json"))
                .unwrap()
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_and_parent_are_rejected_without_touching_victim() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let victim = temp.path().join("victim");
        fs::write(&victim, b"do not touch").unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let linked_parent = temp.path().join("linked");
        symlink(&real, &linked_parent).unwrap();
        assert!(LeaveJournalStore::open(&linked_parent.join("profile.json")).is_err());

        let (binding, metadata) = binding(temp.path());
        let path = sibling_path(&metadata).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&victim, &path).unwrap();
        assert!(LeaveJournalStore::open(&metadata).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"do not touch");
        let _ = binding;
    }

    #[cfg(unix)]
    #[test]
    fn broadly_writable_profile_directory_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (_binding, metadata) = binding(temp.path());
        fs::set_permissions(
            metadata.parent().unwrap(),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(matches!(
            LeaveJournalStore::open(&metadata),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(windows)]
    #[test]
    fn pinned_parent_and_exact_delete_block_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        store.store(&intent(&binding)).unwrap();
        let parent = store.path.parent().unwrap();
        let moved = temp.path().join("moved-profiles");
        assert!(fs::rename(parent, &moved).is_err());

        let held = store.open_target_for_delete().unwrap().unwrap();
        let replacement = temp.path().join("replacement.json");
        fs::write(&replacement, b"attacker").unwrap();
        assert!(fs::rename(&replacement, &store.path).is_err());
        assert!(fs::remove_file(&store.path).is_err());
        psst_platform_security::delete_held_file(&held).unwrap();
        drop(held);
        assert!(!store.path.exists());
        assert_eq!(fs::read(replacement).unwrap(), b"attacker");
    }

    #[cfg(windows)]
    #[test]
    fn broad_inheritable_parent_is_overridden_and_ancestor_retarget_is_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let profile_dir = state.join("profiles");
        fs::create_dir_all(&profile_dir).unwrap();
        let status = ChildProcess::new("icacls.exe")
            .arg(&profile_dir)
            .args(["/inheritance:e", "/grant", "*S-1-1-0:(OI)(CI)F", "/q"])
            .status()
            .unwrap();
        assert!(status.success());
        let metadata = profile_dir.join("default.json");
        let binding = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:7341".into(),
            "alpha".into(),
            "sqd_alpha".into(),
            "mem_worker".into(),
        )
        .unwrap();
        let store = LeaveJournalStore::open(&metadata).unwrap();
        store.store(&intent(&binding)).unwrap();
        let held = store.open_target().unwrap().unwrap();
        psst_platform_security::verify_restricted_file(
            &held,
            &psst_platform_security::current_process_sid().unwrap(),
        )
        .unwrap();
        assert!(fs::rename(&state, temp.path().join("moved-state")).is_err());
        assert!(fs::read_dir(&profile_dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')
        }));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess crash fixture"]
    fn abrupt_store_child() {
        let metadata = PathBuf::from(std::env::var_os("PSST_JOURNAL_CRASH_METADATA").unwrap());
        let binding = ProfileBinding::new(
            "default".into(),
            "http://127.0.0.1:7341".into(),
            "alpha".into(),
            "sqd_alpha".into(),
            "mem_worker".into(),
        )
        .unwrap();
        LeaveJournalStore::open(&metadata)
            .unwrap()
            .store_with_fault(&intent(&binding), JournalFault::CrashAfterPostReplaceSync)
            .unwrap();
        panic!("crash fault did not abort");
    }

    #[cfg(windows)]
    #[test]
    fn post_rename_flush_survives_abrupt_child_restart() {
        let temp = tempfile::tempdir().unwrap();
        let (binding, metadata) = binding(temp.path());
        let status = ChildProcess::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "leave_journal::tests::abrupt_store_child",
            ])
            .env("PSST_JOURNAL_CRASH_METADATA", &metadata)
            .status()
            .unwrap();
        assert!(!status.success());
        let store = LeaveJournalStore::open(&metadata).unwrap();
        assert_eq!(store.load(&binding).unwrap(), Some(intent(&binding)));
        store.store(&intent(&binding)).unwrap();
        assert!(store.load(&binding).unwrap().is_some());
    }
}
