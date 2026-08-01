use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(not(target_os = "linux"))]
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rand::random;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tar::{Archive, Builder, EntryType, Header};
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::compose::{Manager, services};

const MIN_SCHEMA_VERSION: u32 = 1;
const SCHEMA_VERSION: u32 = 3;
const MANIFEST_PATH: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BackupPolicy {
    pub directory: PathBuf,
    pub interval: Duration,
    pub retention: usize,
    pub max_import_bytes: u64,
    pub max_uncompressed_bytes: u64,
}

impl BackupPolicy {
    pub fn new(directory: PathBuf, interval: Duration, retention: usize) -> Self {
        Self {
            directory,
            interval,
            retention: retention.max(2),
            max_import_bytes: 8 * 1024_u64.pow(3),
            max_uncompressed_bytes: 64 * 1024_u64.pow(3),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BackupStore {
    policy: BackupPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlFile {
    pub kind: String,
    pub original_path: String,
    pub archive_path: String,
    pub present: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleMount {
    pub id: String,
    #[serde(default = "default_mount_type")]
    pub mount_type: String,
    pub source: String,
    pub services: Vec<String>,
    pub targets: Vec<String>,
    pub source_type: String,
    pub archive_path: String,
    pub present: bool,
    #[serde(default)]
    pub owner_uid: Option<u32>,
    #[serde(default)]
    pub owner_gid: Option<u32>,
    #[serde(skip, default)]
    backup_source: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcludedMount {
    pub source: String,
    pub services: Vec<String>,
    pub targets: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcludedPath {
    pub mount_id: String,
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceIntent {
    pub name: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupManifest {
    pub schema_version: u32,
    pub application: String,
    pub application_version: String,
    pub created_at: String,
    pub host: String,
    pub project: Option<String>,
    pub kind: String,
    pub consistency: String,
    pub control_files: Vec<ControlFile>,
    pub services: Vec<ServiceIntent>,
    pub mounts: Vec<BundleMount>,
    pub excluded_mounts: Vec<ExcludedMount>,
    #[serde(default)]
    pub excluded_paths: Vec<ExcludedPath>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupRecord {
    pub id: String,
    pub file_name: String,
    pub created_at: String,
    pub kind: String,
    pub consistency: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub service_count: usize,
    pub mount_count: usize,
    pub sensitive: bool,
    #[serde(skip, default)]
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleInspection {
    pub file_name: String,
    pub compressed_size: u64,
    pub sha256: String,
    pub manifest: SetupManifest,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored_at: String,
    pub pre_restore_backup_id: String,
    pub restored_mounts: usize,
    pub restored_services: usize,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
struct MountCandidate {
    source: String,
    mount_type: String,
    services: BTreeSet<String>,
    targets: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct BackupSelection {
    included: Vec<BundleMount>,
    excluded: Vec<ExcludedMount>,
}

#[derive(Debug)]
struct PathRollback {
    destination: PathBuf,
    previous: Option<PathBuf>,
    identity: Option<RestoreIdentity>,
}

#[derive(Clone, Debug)]
struct RestoreIdentity {
    uid: u32,
    gid: u32,
    nfs: bool,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct FilesystemIdentityGuard {
    previous_uid: libc::c_int,
    previous_gid: libc::c_int,
}

#[cfg(target_os = "linux")]
impl FilesystemIdentityGuard {
    fn for_nfs_path(path: &Path) -> Result<Option<Self>> {
        if !is_nfs_path(path)? {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("could not inspect owner of {}", path.display()))?;
        Ok(Some(Self::for_ids(metadata.uid(), metadata.gid())?))
    }

    fn for_ids(requested_uid: u32, requested_gid: u32) -> Result<Self> {
        let requested_uid_value = libc::c_int::try_from(requested_uid)
            .context("filesystem user ID is outside the supported Linux range")?;
        let requested_gid_value = libc::c_int::try_from(requested_gid)
            .context("filesystem group ID is outside the supported Linux range")?;

        // Linux fsuid/fsgid are thread-local. They let a root-run backup read a
        // root-squashed NFS mount with the same identity as the service that owns
        // it, without weakening private configuration file permissions.
        let previous_gid = unsafe { libc::setfsgid(requested_gid) };
        let effective_gid = unsafe { libc::setfsgid(requested_gid) };
        if effective_gid != requested_gid_value {
            unsafe { libc::setfsgid(previous_gid as libc::gid_t) };
            bail!("could not assume filesystem group {requested_gid}");
        }

        let previous_uid = unsafe { libc::setfsuid(requested_uid) };
        let effective_uid = unsafe { libc::setfsuid(requested_uid) };
        if effective_uid != requested_uid_value {
            unsafe {
                libc::setfsuid(previous_uid as libc::uid_t);
                libc::setfsgid(previous_gid as libc::gid_t);
            }
            bail!("could not assume filesystem user {requested_uid}");
        }

        Ok(Self {
            previous_uid,
            previous_gid,
        })
    }
}

#[cfg(target_os = "linux")]
impl Drop for FilesystemIdentityGuard {
    fn drop(&mut self) {
        unsafe {
            libc::setfsuid(self.previous_uid as libc::uid_t);
            libc::setfsgid(self.previous_gid as libc::gid_t);
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct FilesystemIdentityGuard;

#[cfg(not(target_os = "linux"))]
impl FilesystemIdentityGuard {
    fn for_nfs_path(_path: &Path) -> Result<Option<Self>> {
        Ok(None)
    }

    fn for_ids(_uid: u32, _gid: u32) -> Result<Self> {
        Ok(Self)
    }
}

#[cfg(target_os = "linux")]
fn is_nfs_path(path: &Path) -> Result<bool> {
    let path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains a null byte: {}", path.display()))?;
    let mut statistics = MaybeUninit::<libc::statfs>::uninit();
    let status = unsafe { libc::statfs(path.as_ptr(), statistics.as_mut_ptr()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error()).context("could not identify filesystem");
    }
    let statistics = unsafe { statistics.assume_init() };
    Ok(statistics.f_type as u64 == libc::NFS_SUPER_MAGIC as u64)
}

#[cfg(not(target_os = "linux"))]
fn is_nfs_path(_path: &Path) -> Result<bool> {
    Ok(false)
}

fn default_mount_type() -> String {
    "bind".to_string()
}

impl BackupStore {
    pub fn new(policy: BackupPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &BackupPolicy {
        &self.policy
    }

    pub fn ensure_directories(&self) -> Result<()> {
        fs::create_dir_all(&self.policy.directory).with_context(|| {
            format!(
                "could not create backup directory {}",
                self.policy.directory.display()
            )
        })?;
        set_mode_if_possible(&self.policy.directory, 0o700);
        let staging = self.staging_directory();
        fs::create_dir_all(&staging)
            .with_context(|| format!("could not create {}", staging.display()))?;
        set_mode_if_possible(&staging, 0o700);
        Ok(())
    }

    pub fn staging_directory(&self) -> PathBuf {
        self.policy.directory.join(".restore-staging")
    }

    pub fn create_staging_file(&self) -> Result<(PathBuf, File)> {
        self.ensure_directories()?;
        let path = self
            .staging_directory()
            .join(format!("import-{:016x}.tar.gz", random::<u64>()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("could not create {}", path.display()))?;
        Ok((path, file))
    }

    pub fn create_bundle<F>(
        &self,
        manager: &Manager,
        kind: &str,
        mut progress: F,
    ) -> Result<BackupRecord>
    where
        F: FnMut(String),
    {
        self.ensure_directories()?;
        let config = manager.compose_config()?;
        let disabled = manager.read_disabled_services();
        let mut selection = select_backup_mounts(&config, &self.policy.directory)?;
        resolve_backup_sources(manager, &config, &mut selection.included)?;
        let now = Utc::now();
        let id = format!("{}-{:08x}", now.format("%Y%m%dT%H%M%SZ"), random::<u32>());
        let file_name = format!("setup-{id}.tar.gz");
        let final_path = self.policy.directory.join(&file_name);
        let partial_path = self.policy.directory.join(format!(".{file_name}.partial"));
        let control_files = control_files(manager);
        let service_intent = services(&config)
            .map(|items| {
                items
                    .keys()
                    .map(|name| ServiceIntent {
                        name: name.clone(),
                        disabled: disabled.contains(name),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut manifest = SetupManifest {
            schema_version: SCHEMA_VERSION,
            application: "docker-services".to_string(),
            application_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: now.to_rfc3339(),
            host: manager.host().to_string(),
            project: config
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            kind: kind.to_string(),
            consistency: "online".to_string(),
            control_files,
            services: service_intent,
            mounts: selection.included,
            excluded_mounts: selection.excluded,
            excluded_paths: Vec::new(),
        };

        progress(format!(
            "Archiving {} configuration mount(s); media and cache paths stay excluded",
            manifest.mounts.iter().filter(|mount| mount.present).count()
        ));
        let operation = (|| -> Result<()> {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&partial_path)
                .with_context(|| format!("could not create {}", partial_path.display()))?;
            let encoder = GzEncoder::new(BufWriter::new(file), Compression::new(3));
            let mut archive = Builder::new(encoder);
            archive.follow_symlinks(false);
            for control in &manifest.control_files {
                if !control.present {
                    continue;
                }
                let source = Path::new(&control.original_path);
                progress(format!("Saving {}", control.kind));
                archive
                    .append_path_with_name(source, &control.archive_path)
                    .with_context(|| format!("could not archive {}", source.display()))?;
            }
            for mount in &manifest.mounts {
                if !mount.present {
                    continue;
                }
                let source = &mount.backup_source;
                progress(format!(
                    "Saving {} for {}",
                    mount.targets.join(", "),
                    mount.services.join(", ")
                ));
                {
                    let _identity = FilesystemIdentityGuard::for_nfs_path(source)?;
                    if mount.source_type == "directory" {
                        append_mount_directory(
                            &mut archive,
                            mount,
                            source,
                            &mut manifest.excluded_paths,
                        )
                        .with_context(|| format!("could not archive {}", source.display()))?;
                    } else {
                        archive
                            .append_path_with_name(source, &mount.archive_path)
                            .with_context(|| format!("could not archive {}", source.display()))?;
                    }
                }
            }
            let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
            append_bytes(&mut archive, MANIFEST_PATH, &manifest_bytes, 0o600)?;
            let encoder = archive.into_inner()?;
            let mut writer = encoder.finish()?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::rename(&partial_path, &final_path)?;
            set_mode_if_possible(&final_path, 0o600);
            Ok(())
        })();
        if let Err(error) = operation {
            let _ = fs::remove_file(&partial_path);
            return Err(error);
        }

        let size_bytes = fs::metadata(&final_path)?.len();
        let sha256 = sha256_file(&final_path)?;
        let record = BackupRecord {
            id,
            file_name,
            created_at: manifest.created_at.clone(),
            kind: manifest.kind.clone(),
            consistency: manifest.consistency.clone(),
            size_bytes,
            sha256,
            service_count: manifest.services.len(),
            mount_count: manifest.mounts.iter().filter(|mount| mount.present).count(),
            sensitive: true,
            path: final_path,
        };
        self.write_record(&record)?;
        let removed = self.prune()?;
        if removed > 0 {
            progress(format!("Retention removed {removed} expired bundle(s)"));
        }
        progress(format!(
            "Setup bundle complete: {} ({})",
            record.file_name,
            human_bytes(record.size_bytes)
        ));
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<BackupRecord>> {
        if !self.policy.directory.is_dir() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.policy.directory)? {
            let entry = entry?;
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".tar.gz.json"))
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut record) = serde_json::from_str::<BackupRecord>(&text) else {
                continue;
            };
            let bundle = self.policy.directory.join(&record.file_name);
            if bundle.is_file() {
                record.path = bundle;
                records.push(record);
            }
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub fn find(&self, id: &str) -> Result<BackupRecord> {
        validate_id(id)?;
        self.list()?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| anyhow!("setup bundle {id:?} was not found"))
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let record = self.find(id)?;
        let sidecar = sidecar_path(&record.path);
        fs::remove_file(&record.path)
            .with_context(|| format!("could not delete {}", record.path.display()))?;
        if sidecar.exists() {
            fs::remove_file(&sidecar)?;
        }
        Ok(())
    }

    pub fn prune_retention(&self) -> Result<usize> {
        self.prune()
    }

    pub fn due(&self, now: DateTime<Utc>) -> Result<bool> {
        let Some(latest) = self.list()?.into_iter().next() else {
            return Ok(true);
        };
        let created = DateTime::parse_from_rfc3339(&latest.created_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        let elapsed = now.signed_duration_since(created);
        Ok(elapsed
            .to_std()
            .map(|duration| duration >= self.policy.interval)
            .unwrap_or(false))
    }

    pub fn inspect(&self, path: &Path) -> Result<BundleInspection> {
        let metadata =
            fs::metadata(path).with_context(|| format!("could not inspect {}", path.display()))?;
        if metadata.len() > self.policy.max_import_bytes {
            bail!(
                "bundle is {}; the import limit is {}",
                human_bytes(metadata.len()),
                human_bytes(self.policy.max_import_bytes)
            );
        }
        let file = File::open(path)?;
        let decoder = GzDecoder::new(BufReader::new(file));
        let mut archive = Archive::new(decoder);
        let mut manifest = None;
        let mut entry_count = 0_u64;
        let mut unpacked_bytes = 0_u64;
        for entry in archive
            .entries()
            .context("bundle is not a readable tar archive")?
        {
            let mut entry = entry?;
            entry_count += 1;
            if entry_count > 2_000_000 {
                bail!("bundle contains too many archive entries");
            }
            let path = entry.path()?.into_owned();
            ensure_safe_archive_path(&path)?;
            ensure_allowed_archive_path(&path)?;
            unpacked_bytes = unpacked_bytes
                .checked_add(entry.header().size()?)
                .ok_or_else(|| anyhow!("bundle expanded size overflow"))?;
            if unpacked_bytes > self.policy.max_uncompressed_bytes {
                bail!(
                    "bundle expands beyond the {} safety limit",
                    human_bytes(self.policy.max_uncompressed_bytes)
                );
            }
            validate_entry_type(&entry, &path)?;
            if path == Path::new(MANIFEST_PATH) {
                if manifest.is_some() {
                    bail!("bundle contains more than one manifest");
                }
                if entry.header().size()? > MAX_MANIFEST_BYTES {
                    bail!("bundle manifest is too large");
                }
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                manifest = Some(
                    serde_json::from_slice::<SetupManifest>(&bytes)
                        .context("bundle manifest is invalid")?,
                );
            }
        }
        let manifest = manifest.ok_or_else(|| anyhow!("bundle has no manifest.json"))?;
        validate_manifest(&manifest)?;
        let mut warnings = vec![
            "This setup bundle contains private service configuration and may contain credentials."
                .to_string(),
        ];
        if manifest.consistency == "online" {
            warnings.push(
                "The bundle was captured online; database files may reflect an in-flight write."
                    .to_string(),
            );
        }
        if !manifest.excluded_mounts.is_empty() {
            warnings.push(format!(
                "{} media, cache, log, socket, or generated-data mount(s) are intentionally excluded.",
                manifest.excluded_mounts.len()
            ));
        }
        if !manifest.excluded_paths.is_empty() {
            warnings.push(format!(
                "{} transient, socket, or unsafe-symlink path(s) are intentionally excluded and will be recreated by their services.",
                manifest.excluded_paths.len()
            ));
        }
        Ok(BundleInspection {
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("setup-bundle.tar.gz")
                .to_string(),
            compressed_size: metadata.len(),
            sha256: sha256_file(path)?,
            manifest,
            warnings,
        })
    }

    pub fn restore<F>(
        &self,
        manager: &Manager,
        bundle_path: &Path,
        mut progress: F,
    ) -> Result<RestoreReport>
    where
        F: FnMut(String),
    {
        let inspection = self.inspect(bundle_path)?;
        progress(format!(
            "Verified {} · SHA-256 {}",
            inspection.file_name,
            &inspection.sha256[..12]
        ));
        self.ensure_directories()?;
        let staging = tempfile::Builder::new()
            .prefix("restore-")
            .tempdir_in(&self.policy.directory)?;
        extract_bundle(bundle_path, &staging, self.policy.max_uncompressed_bytes)?;
        validate_staged_bundle(manager, &inspection.manifest, staging.path())?;

        progress("Creating an online pre-restore recovery bundle".to_string());
        let pre_restore = self.create_bundle(manager, "pre-restore", &mut progress)?;
        progress("Stopping the current Compose stack for a consistent restore".to_string());
        manager.stop_stack(&mut progress)?;

        let mut rollbacks = Vec::new();
        let operation = (|| -> Result<()> {
            for control in &inspection.manifest.control_files {
                let destination = match control.kind.as_str() {
                    "compose" => manager.compose_file().to_path_buf(),
                    "environment" => manager.env_file(),
                    "disabled-services" => manager.disabled_state_path(),
                    _ => bail!("unsupported control file kind {:?}", control.kind),
                };
                let source = staging.path().join(&control.archive_path);
                if control.kind == "compose" && !control.present {
                    bail!("setup bundle does not contain its Compose file");
                }
                progress(format!("Restoring {}", control.kind));
                rollbacks.push(if control.present {
                    replace_path(&source, &destination, &inspection.sha256[..12], None)?
                } else {
                    remove_with_rollback(&destination, &inspection.sha256[..12], None)?
                });
            }

            manager.validate_compose()?;
            let restored_config = manager.compose_config()?;
            for mount in inspection
                .manifest
                .mounts
                .iter()
                .filter(|mount| mount.present)
            {
                let destination = match mount.mount_type.as_str() {
                    "bind" => {
                        let destination = PathBuf::from(&mount.source);
                        ensure_safe_restore_destination(&destination, &self.policy.directory)?;
                        destination
                    }
                    "volume" => {
                        let volume_name = compose_volume_name(&restored_config, &mount.source)?;
                        manager.ensure_volume_mountpoint(&volume_name)?
                    }
                    other => bail!("unsupported mount type {other:?}"),
                };
                let source = staging.path().join(&mount.archive_path);
                progress(format!(
                    "Restoring {} for {}",
                    mount.targets.join(", "),
                    mount.services.join(", ")
                ));
                let identity = restore_identity(mount, &destination)?;
                rollbacks.push(replace_path(
                    &source,
                    &destination,
                    &inspection.sha256[..12],
                    Some(&identity),
                )?);
            }

            manager.start_enabled_services(&mut progress)?;
            Ok(())
        })();

        if let Err(error) = operation {
            progress(format!("Restore failed; rolling back: {error:#}"));
            let rollback_result = rollback_paths(&mut rollbacks);
            let restart_result = manager
                .validate_compose()
                .and_then(|_| manager.start_enabled_services(&mut progress));
            if let Err(rollback_error) = rollback_result {
                return Err(error.context(format!(
                    "automatic rollback also failed: {rollback_error:#}"
                )));
            }
            if let Err(restart_error) = restart_result {
                return Err(error.context(format!(
                    "previous files were restored, but the old stack did not restart: {restart_error:#}"
                )));
            }
            return Err(error.context("restore failed; previous files and services were restored"));
        }

        cleanup_rollbacks(&mut rollbacks)?;
        Ok(RestoreReport {
            restored_at: Utc::now().to_rfc3339(),
            pre_restore_backup_id: pre_restore.id,
            restored_mounts: inspection
                .manifest
                .mounts
                .iter()
                .filter(|mount| mount.present)
                .count(),
            restored_services: inspection.manifest.services.len(),
            message: "Configuration restored, validated, and enabled services started".to_string(),
        })
    }

    fn write_record(&self, record: &BackupRecord) -> Result<()> {
        let path = sidecar_path(&record.path);
        let mut bytes = serde_json::to_vec_pretty(record)?;
        bytes.push(b'\n');
        atomic_write(&path, &bytes, 0o600)
    }

    fn prune(&self) -> Result<usize> {
        let records = self.list()?;
        let mut removed = 0;
        for record in records.into_iter().skip(self.policy.retention) {
            self.delete(&record.id)?;
            removed += 1;
        }
        Ok(removed)
    }
}

fn control_files(manager: &Manager) -> Vec<ControlFile> {
    [
        (
            "compose",
            manager.compose_file().to_path_buf(),
            "control/docker-compose.yml",
        ),
        ("environment", manager.env_file(), "control/.env"),
        (
            "disabled-services",
            manager.disabled_state_path(),
            "control/.docker-services-disabled.json",
        ),
    ]
    .into_iter()
    .map(|(kind, path, archive_path)| ControlFile {
        kind: kind.to_string(),
        original_path: path.display().to_string(),
        archive_path: archive_path.to_string(),
        present: path.is_file(),
    })
    .collect()
}

fn select_backup_mounts(config: &Value, backup_directory: &Path) -> Result<BackupSelection> {
    let mut candidates = BTreeMap::<(String, String), MountCandidate>::new();
    if let Some(items) = services(config) {
        for (service_name, service) in items {
            for (source, target, volume_type) in service_volumes(service) {
                if volume_type != "bind" && volume_type != "volume" {
                    continue;
                }
                let key = (volume_type.clone(), source.clone());
                let candidate = candidates.entry(key).or_insert_with(|| MountCandidate {
                    source,
                    mount_type: volume_type,
                    ..MountCandidate::default()
                });
                candidate.services.insert(service_name.clone());
                candidate.targets.insert(target);
            }
        }
    }

    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for (index, candidate) in candidates.into_values().enumerate() {
        let services = candidate.services.into_iter().collect::<Vec<_>>();
        let targets = candidate.targets.into_iter().collect::<Vec<_>>();
        let source_path = PathBuf::from(&candidate.source);
        let (metadata, source_type, archive_leaf, backup_source) =
            if candidate.mount_type == "volume" {
                (None, "directory", "data", PathBuf::new())
            } else {
                if let Some(reason) = exclusion_reason(&source_path, &targets, backup_directory) {
                    excluded.push(ExcludedMount {
                        source: candidate.source,
                        services,
                        targets,
                        reason,
                    });
                    continue;
                }
                let metadata = fs::symlink_metadata(&source_path).ok();
                let (source_type, archive_leaf) = match metadata.as_ref() {
                    Some(metadata) if metadata.is_dir() => ("directory", "data"),
                    Some(metadata) if metadata.is_file() => ("file", "source"),
                    Some(_) => ("unsupported", "source"),
                    None => ("missing", "source"),
                };
                (metadata, source_type, archive_leaf, source_path)
            };
        if source_type == "unsupported" {
            excluded.push(ExcludedMount {
                source: candidate.source,
                services,
                targets,
                reason: "unsupported host path type".to_string(),
            });
            continue;
        }
        let id = format!("{index:04}");
        included.push(BundleMount {
            id: id.clone(),
            mount_type: candidate.mount_type,
            source: candidate.source,
            services,
            targets,
            source_type: source_type.to_string(),
            archive_path: format!("mounts/{id}/{archive_leaf}"),
            present: metadata.is_some(),
            owner_uid: None,
            owner_gid: None,
            backup_source,
        });
    }
    Ok(BackupSelection { included, excluded })
}

fn resolve_backup_sources(
    manager: &Manager,
    config: &Value,
    mounts: &mut [BundleMount],
) -> Result<()> {
    for mount in mounts {
        if mount.mount_type == "bind" {
            mount.backup_source = PathBuf::from(&mount.source);
        } else {
            let volume_name = compose_volume_name(config, &mount.source)?;
            let Some(mountpoint) = manager.volume_mountpoint(&volume_name)? else {
                mount.present = false;
                mount.backup_source = PathBuf::new();
                continue;
            };
            let metadata = fs::symlink_metadata(&mountpoint).with_context(|| {
                format!(
                    "could not inspect Docker volume {volume_name:?} at {}",
                    mountpoint.display()
                )
            })?;
            if !metadata.is_dir() {
                bail!("Docker volume {volume_name:?} does not resolve to a directory");
            }
            mount.present = true;
            mount.source_type = "directory".to_string();
            mount.backup_source = mountpoint;
        }
        if mount.present {
            let metadata = fs::symlink_metadata(&mount.backup_source).with_context(|| {
                format!(
                    "could not inspect owner of {}",
                    mount.backup_source.display()
                )
            })?;
            mount.owner_uid = Some(metadata.uid());
            mount.owner_gid = Some(metadata.gid());
        }
    }
    Ok(())
}

fn compose_volume_name(config: &Value, logical_name: &str) -> Result<String> {
    if let Some(name) = config
        .get("volumes")
        .and_then(|volumes| volumes.get(logical_name))
        .and_then(|volume| volume.get("name"))
        .and_then(Value::as_str)
    {
        return Ok(name.to_string());
    }
    let project = config.get("name").and_then(Value::as_str).ok_or_else(|| {
        anyhow!("Compose did not report a project name for volume {logical_name:?}")
    })?;
    Ok(format!("{project}_{logical_name}"))
}

fn service_volumes(service: &Value) -> Vec<(String, String, String)> {
    service
        .get("volumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|volume| match volume {
            Value::Object(item) => Some((
                item.get("source")?.as_str()?.to_string(),
                item.get("target")?.as_str()?.to_string(),
                item.get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("volume")
                    .to_string(),
            )),
            Value::String(value) => {
                let mut parts = value.split(':');
                let source = parts.next()?.to_string();
                let target = parts.next()?.to_string();
                let volume_type = if source.starts_with('/') {
                    "bind"
                } else {
                    "volume"
                };
                Some((source, target, volume_type.to_string()))
            }
            _ => None,
        })
        .collect()
}

fn exclusion_reason(source: &Path, targets: &[String], backup_directory: &Path) -> Option<String> {
    if source == Path::new("/var/run/docker.sock") {
        return Some("Docker control socket".to_string());
    }
    if backup_directory.starts_with(source) || source.starts_with(backup_directory) {
        return Some("backup storage recursion".to_string());
    }
    let source_lower = source.to_string_lossy().to_ascii_lowercase();
    let source_components = source_lower.split('/').collect::<Vec<_>>();
    let excluded_source_names = [
        "cache",
        "image-cache",
        "downloads",
        "movies",
        "tv-series",
        "media",
        "temp",
        "transcode",
        "logs",
        "log",
        "backups",
        "assetsbackup",
    ];
    if source_components
        .last()
        .is_some_and(|name| excluded_source_names.contains(name))
    {
        return Some("media, cache, log, temporary, or nested backup data".to_string());
    }
    for target in targets {
        let target = target.to_ascii_lowercase();
        let first = target
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or("");
        if [
            "media",
            "downloads",
            "movies",
            "tv",
            "music",
            "comics",
            "audiobooks",
            "podcasts",
            "cache",
            "image-cache",
            "temp",
            "transcode",
            "logs",
            "log",
            "backups",
            "assets",
            "assetsbackup",
        ]
        .contains(&first)
            && first != "manualassets"
        {
            return Some(format!(
                "container target {target} is non-configuration data"
            ));
        }
        if target == "/var/run/docker.sock" || target.starts_with("/dev/") {
            return Some(format!("container target {target} is a socket or device"));
        }
    }
    None
}

fn validate_manifest(manifest: &SetupManifest) -> Result<()> {
    if !(MIN_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&manifest.schema_version) {
        bail!(
            "setup schema {} is unsupported; expected {} through {}",
            manifest.schema_version,
            MIN_SCHEMA_VERSION,
            SCHEMA_VERSION
        );
    }
    if manifest.application != "docker-services" {
        bail!("bundle was not produced by docker-services");
    }
    if !manifest
        .control_files
        .iter()
        .any(|file| file.kind == "compose" && file.present)
    {
        bail!("bundle has no Compose control file");
    }
    let mut ids = BTreeSet::new();
    for mount in &manifest.mounts {
        if mount.mount_type != "bind" && mount.mount_type != "volume" {
            bail!("mount {} has an unsupported type", mount.id);
        }
        if !ids.insert(mount.id.clone()) {
            bail!("bundle repeats mount id {:?}", mount.id);
        }
        validate_id(&mount.id)?;
        let expected_prefix = format!("mounts/{}/", mount.id);
        if !mount.archive_path.starts_with(&expected_prefix) {
            bail!("mount {} has an invalid archive path", mount.id);
        }
        if mount.owner_uid.is_some() != mount.owner_gid.is_some() {
            bail!("mount {} has incomplete ownership metadata", mount.id);
        }
        ensure_safe_archive_path(Path::new(&mount.archive_path))?;
    }
    for excluded in &manifest.excluded_paths {
        if !ids.contains(&excluded.mount_id) {
            bail!(
                "excluded path references unknown mount {}",
                excluded.mount_id
            );
        }
        ensure_safe_archive_path(Path::new(&excluded.path))?;
        if excluded.reason.trim().is_empty() {
            bail!("excluded path {} has no reason", excluded.path);
        }
    }
    for control in &manifest.control_files {
        ensure_safe_archive_path(Path::new(&control.archive_path))?;
        if !control.archive_path.starts_with("control/") {
            bail!(
                "control file {:?} has an invalid archive path",
                control.kind
            );
        }
    }
    Ok(())
}

fn validate_staged_bundle(
    manager: &Manager,
    manifest: &SetupManifest,
    staging: &Path,
) -> Result<()> {
    let compose = manifest
        .control_files
        .iter()
        .find(|item| item.kind == "compose" && item.present)
        .ok_or_else(|| anyhow!("bundle has no Compose file"))?;
    let staged_compose = staging.join(&compose.archive_path);
    let staged_manager = Manager::new(staged_compose, manager.host().to_string())
        .with_project_name(manifest.project.clone());
    let config = staged_manager
        .compose_config()
        .context("bundled Compose file did not validate")?;
    let selected = select_backup_mounts(&config, Path::new("/__bundle_backup_sentinel__"))?;
    let compose_sources = selected
        .included
        .iter()
        .map(|mount| (mount.mount_type.as_str(), mount.source.as_str()))
        .collect::<BTreeSet<_>>();
    for mount in manifest.mounts.iter().filter(|mount| mount.present) {
        if !compose_sources.contains(&(mount.mount_type.as_str(), mount.source.as_str())) {
            bail!(
                "manifest source {} is not a matching mount in the bundled Compose file",
                mount.source
            );
        }
        if mount.mount_type == "bind" {
            ensure_safe_restore_destination(
                Path::new(&mount.source),
                &manager.compose_file().join(".never"),
            )?;
        } else {
            let _ = compose_volume_name(&config, &mount.source)?;
        }
    }
    Ok(())
}

fn extract_bundle(path: &Path, destination: &TempDir, max_bytes: u64) -> Result<()> {
    let file = File::open(path)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = Archive::new(decoder);
    let mut expanded = 0_u64;
    let mut symlinks = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        ensure_safe_archive_path(&relative)?;
        ensure_allowed_archive_path(&relative)?;
        validate_entry_type(&entry, &relative)?;
        expanded = expanded
            .checked_add(entry.header().size()?)
            .ok_or_else(|| anyhow!("bundle expanded size overflow"))?;
        if expanded > max_bytes {
            bail!("bundle exceeds the expanded-size limit");
        }
        let output = destination.path().join(&relative);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&output)?;
            set_mode_if_possible(&output, entry.header().mode().unwrap_or(0o700) & 0o777);
        } else if entry_type.is_file() || entry_type.is_gnu_sparse() {
            let parent = output
                .parent()
                .ok_or_else(|| anyhow!("invalid archive path {}", relative.display()))?;
            fs::create_dir_all(parent)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(entry.header().mode().unwrap_or(0o600) & 0o777)
                .open(&output)?;
            std::io::copy(&mut entry, &mut file)?;
            file.sync_all()?;
        } else if entry_type.is_symlink() {
            let target = entry
                .link_name()?
                .ok_or_else(|| anyhow!("symlink {} has no target", relative.display()))?
                .into_owned();
            ensure_safe_symlink(&relative, &target)?;
            symlinks.push((output, target));
        }
    }
    for (output, target) in symlinks {
        let parent = output
            .parent()
            .ok_or_else(|| anyhow!("invalid symlink path {}", output.display()))?;
        fs::create_dir_all(parent)?;
        symlink(&target, &output)?;
    }
    Ok(())
}

fn validate_entry_type<R: Read>(entry: &tar::Entry<'_, R>, path: &Path) -> Result<()> {
    let entry_type = entry.header().entry_type();
    if !supported_entry_type(entry_type) {
        bail!("archive entry {} has an unsupported type", path.display());
    }
    if entry_type.is_symlink() {
        let target = entry
            .link_name()?
            .ok_or_else(|| anyhow!("symlink {} has no target", path.display()))?;
        ensure_safe_symlink(path, &target)?;
    }
    Ok(())
}

fn supported_entry_type(entry_type: EntryType) -> bool {
    entry_type.is_file()
        || entry_type.is_gnu_sparse()
        || entry_type.is_dir()
        || entry_type.is_symlink()
}

fn ensure_safe_symlink(path: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        bail!("symlink {} has an absolute target", path.display());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let mut depth = parent.components().count() as isize;
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    bail!("symlink {} escapes the bundle", path.display());
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("symlink {} has an unsafe target", path.display())
            }
        }
    }
    Ok(())
}

fn ensure_safe_archive_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("archive contains an invalid absolute or empty path");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!(
            "archive path {} attempts directory traversal",
            path.display()
        );
    }
    Ok(())
}

fn ensure_allowed_archive_path(path: &Path) -> Result<()> {
    if path == Path::new(MANIFEST_PATH) || path.starts_with("control") || path.starts_with("mounts")
    {
        return Ok(());
    }
    bail!(
        "archive path {} is outside the setup schema",
        path.display()
    )
}

fn ensure_safe_restore_destination(path: &Path, backup_directory: &Path) -> Result<()> {
    if !path.is_absolute() || !path.starts_with("/nfs") || path == Path::new("/nfs") {
        bail!(
            "restore destination {} is outside the permitted /nfs service tree",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("restore destination {} is unsafe", path.display());
    }
    if path.starts_with(backup_directory) || backup_directory.starts_with(path) {
        bail!("restore destination overlaps protected backup storage");
    }
    let ancestor = nearest_existing_ancestor(path)?;
    let canonical = ancestor.canonicalize()?;
    if !canonical.starts_with("/nfs") {
        bail!(
            "restore destination {} resolves outside /nfs",
            path.display()
        );
    }
    Ok(())
}

fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf> {
    let mut candidate = path.to_path_buf();
    while !candidate.exists() {
        if !candidate.pop() {
            bail!("restore destination has no existing ancestor");
        }
    }
    Ok(candidate)
}

fn restore_identity(mount: &BundleMount, destination: &Path) -> Result<RestoreIdentity> {
    let existing = fs::symlink_metadata(destination).ok();
    let uid = mount
        .owner_uid
        .or_else(|| existing.as_ref().map(MetadataExt::uid))
        .ok_or_else(|| {
            anyhow!(
                "mount {} has no saved owner and {} does not exist",
                mount.id,
                destination.display()
            )
        })?;
    let gid = mount
        .owner_gid
        .or_else(|| existing.as_ref().map(MetadataExt::gid))
        .ok_or_else(|| {
            anyhow!(
                "mount {} has no saved group and {} does not exist",
                mount.id,
                destination.display()
            )
        })?;
    let filesystem_path = nearest_existing_ancestor(destination)?;
    Ok(RestoreIdentity {
        uid,
        gid,
        nfs: is_nfs_path(&filesystem_path)?,
    })
}

fn replace_path(
    source: &Path,
    destination: &Path,
    suffix: &str,
    identity: Option<&RestoreIdentity>,
) -> Result<PathRollback> {
    if !source.exists() && fs::symlink_metadata(source).is_err() {
        bail!("staged restore path {} is missing", source.display());
    }
    let rollback = move_aside(destination, suffix, identity)?;
    if let Err(error) = copy_path(source, destination, identity) {
        let _ = remove_path_with_identity(destination, identity);
        if let Some(previous) = &rollback.previous {
            let _ = run_as_destination_owner(identity, || {
                fs::rename(previous, destination).map_err(anyhow::Error::from)
            });
        }
        return Err(error);
    }
    Ok(rollback)
}

fn remove_with_rollback(
    destination: &Path,
    suffix: &str,
    identity: Option<&RestoreIdentity>,
) -> Result<PathRollback> {
    move_aside(destination, suffix, identity)
}

fn move_aside(
    destination: &Path,
    suffix: &str,
    identity: Option<&RestoreIdentity>,
) -> Result<PathRollback> {
    let previous = if fs::symlink_metadata(destination).is_ok() {
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("destination {} has no parent", destination.display()))?;
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("invalid destination {}", destination.display()))?;
        let candidate = parent.join(format!(".{name}.pre-restore-{suffix}"));
        if candidate.exists() {
            bail!("rollback path {} already exists", candidate.display());
        }
        run_as_destination_owner(identity, || {
            fs::create_dir_all(parent)?;
            fs::rename(destination, &candidate).with_context(|| {
                format!("could not move {} aside for restore", destination.display())
            })
        })?;
        Some(candidate)
    } else {
        None
    };
    Ok(PathRollback {
        destination: destination.to_path_buf(),
        previous,
        identity: identity.cloned(),
    })
}

fn rollback_paths(rollbacks: &mut Vec<PathRollback>) -> Result<()> {
    let mut errors = Vec::new();
    for rollback in rollbacks.iter().rev() {
        if let Err(error) =
            remove_path_with_identity(&rollback.destination, rollback.identity.as_ref())
        {
            errors.push(format!(
                "remove {}: {error}",
                rollback.destination.display()
            ));
            continue;
        }
        if let Some(previous) = &rollback.previous
            && let Err(error) = run_as_destination_owner(rollback.identity.as_ref(), || {
                fs::rename(previous, &rollback.destination).map_err(anyhow::Error::from)
            })
        {
            errors.push(format!(
                "restore {}: {error}",
                rollback.destination.display()
            ));
        }
    }
    rollbacks.clear();
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("{}", errors.join("; "))
    }
}

fn cleanup_rollbacks(rollbacks: &mut Vec<PathRollback>) -> Result<()> {
    for rollback in rollbacks.iter() {
        if let Some(previous) = &rollback.previous {
            remove_path_with_identity(previous, rollback.identity.as_ref())?;
        }
    }
    rollbacks.clear();
    Ok(())
}

fn copy_path(source: &Path, destination: &Path, identity: Option<&RestoreIdentity>) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        for entry in WalkDir::new(source).follow_links(false) {
            let entry = entry?;
            let relative = entry.path().strip_prefix(source)?;
            let output = destination.join(relative);
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                create_destination_directory(&output, &metadata, identity)?;
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(entry.path())?;
                ensure_safe_symlink(relative, &target)?;
                create_destination_symlink(&output, &target, identity)?;
            } else if metadata.is_file() {
                copy_destination_file(entry.path(), &output, &metadata, identity)?;
            } else {
                bail!("unsupported file type at {}", entry.path().display());
            }
        }
    } else if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        ensure_safe_symlink(Path::new("source"), &target)?;
        create_destination_symlink(destination, &target, identity)?;
    } else if metadata.is_file() {
        copy_destination_file(source, destination, &metadata, identity)?;
    } else {
        bail!("unsupported staged path {}", source.display());
    }
    Ok(())
}

fn create_destination_directory(
    output: &Path,
    metadata: &fs::Metadata,
    identity: Option<&RestoreIdentity>,
) -> Result<()> {
    run_as_destination_owner(identity, || {
        fs::create_dir_all(output)?;
        fs::set_permissions(output, metadata.permissions())?;
        Ok(())
    })?;
    set_destination_owner(output, identity, false)
}

fn create_destination_symlink(
    output: &Path,
    target: &Path,
    identity: Option<&RestoreIdentity>,
) -> Result<()> {
    let parent = output
        .parent()
        .ok_or_else(|| anyhow!("invalid destination"))?;
    run_as_destination_owner(identity, || {
        fs::create_dir_all(parent)?;
        symlink(target, output)?;
        Ok(())
    })?;
    set_destination_owner(output, identity, true)
}

fn copy_destination_file(
    source: &Path,
    output: &Path,
    metadata: &fs::Metadata,
    identity: Option<&RestoreIdentity>,
) -> Result<()> {
    let mut input = File::open(source)?;
    let parent = output
        .parent()
        .ok_or_else(|| anyhow!("invalid destination"))?;
    run_as_destination_owner(identity, || {
        fs::create_dir_all(parent)?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(metadata.permissions().mode())
            .open(output)?;
        std::io::copy(&mut input, &mut destination)?;
        fs::set_permissions(output, metadata.permissions())?;
        Ok(())
    })?;
    set_destination_owner(output, identity, false)
}

fn run_as_destination_owner<T>(
    identity: Option<&RestoreIdentity>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _guard = identity
        .filter(|identity| identity.nfs)
        .map(|identity| FilesystemIdentityGuard::for_ids(identity.uid, identity.gid))
        .transpose()?;
    operation()
}

#[cfg(target_os = "linux")]
fn set_destination_owner(
    path: &Path,
    identity: Option<&RestoreIdentity>,
    symlink_path: bool,
) -> Result<()> {
    let Some(identity) = identity.filter(|identity| !identity.nfs) else {
        return Ok(());
    };
    let path_bytes = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains a null byte: {}", path.display()))?;
    let status = unsafe {
        if symlink_path {
            libc::lchown(path_bytes.as_ptr(), identity.uid, identity.gid)
        } else {
            libc::chown(path_bytes.as_ptr(), identity.uid, identity.gid)
        }
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not restore owner of {}", path.display()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn set_destination_owner(
    _path: &Path,
    _identity: Option<&RestoreIdentity>,
    _symlink_path: bool,
) -> Result<()> {
    Ok(())
}

fn remove_path_with_identity(path: &Path, identity: Option<&RestoreIdentity>) -> Result<()> {
    run_as_destination_owner(identity, || remove_path(path))
}

fn remove_path(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn append_mount_directory<W: Write>(
    archive: &mut Builder<W>,
    mount: &BundleMount,
    source: &Path,
    excluded: &mut Vec<ExcludedPath>,
) -> Result<()> {
    let mut entries = WalkDir::new(source).follow_links(false).into_iter();
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let file_type = entry.file_type();
        if let Some(reason) = transient_entry_reason(relative, file_type) {
            if file_type.is_dir() {
                entries.skip_current_dir();
            }
            record_excluded_path(excluded, mount, relative, reason);
            continue;
        }

        if file_type.is_symlink() {
            let target = fs::read_link(entry.path())?;
            if ensure_safe_symlink(relative, &target).is_err() {
                record_excluded_path(
                    excluded,
                    mount,
                    relative,
                    "absolute or escaping symlink target".to_string(),
                );
                continue;
            }
        } else if !(file_type.is_dir() || file_type.is_file()) {
            record_excluded_path(
                excluded,
                mount,
                relative,
                "runtime socket, device, or unsupported special file".to_string(),
            );
            continue;
        }

        let archive_path = Path::new(&mount.archive_path).join(relative);
        let result = if file_type.is_dir() {
            archive.append_dir(&archive_path, entry.path())
        } else {
            archive.append_path_with_name(entry.path(), &archive_path)
        };
        if let Err(error) = result {
            if error.kind() == std::io::ErrorKind::NotFound {
                record_excluded_path(
                    excluded,
                    mount,
                    relative,
                    "entry disappeared during online capture".to_string(),
                );
                continue;
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn transient_entry_reason(relative: &Path, file_type: fs::FileType) -> Option<String> {
    if relative.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("#innodb_temp")
    }) {
        return Some("database temporary workspace".to_string());
    }
    let name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if file_type.is_file() && name.ends_with(".pid") {
        return Some("runtime process marker".to_string());
    }
    None
}

fn record_excluded_path(
    excluded: &mut Vec<ExcludedPath>,
    mount: &BundleMount,
    relative: &Path,
    reason: String,
) {
    let path = relative.to_string_lossy().into_owned();
    if path.is_empty() {
        return;
    }
    excluded.push(ExcludedPath {
        mount_id: mount.id.clone(),
        path,
        reason,
    });
}

fn append_bytes<W: Write>(
    archive: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sidecar_path(bundle: &Path) -> PathBuf {
    PathBuf::from(format!("{}.json", bundle.display()))
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid output path {}", path.display()))?;
    let temporary = parent.join(format!(".{name}.tmp-{:08x}", random::<u32>()));
    let operation = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        set_mode_if_possible(path, mode);
        Ok(())
    })();
    if operation.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    operation
}

fn set_mode_if_possible(path: &Path, mode: u32) {
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 80
        || !id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || value == b'-' || value == b'_')
    {
        bail!("invalid setup bundle identifier");
    }
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn mount_selection_excludes_libraries_caches_and_socket() {
        let directory = tempdir().unwrap();
        let config_dir = directory.path().join("app/config");
        let media_dir = directory.path().join("media");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&media_dir).unwrap();
        let config = json!({
            "services": {
                "app": {"volumes": [
                    {"type":"bind", "source": config_dir, "target":"/config"},
                    {"type":"bind", "source": media_dir, "target":"/media"},
                    {"type":"bind", "source":"/var/run/docker.sock", "target":"/var/run/docker.sock"}
                ]}
            }
        });
        let selection = select_backup_mounts(&config, &directory.path().join("backups")).unwrap();
        assert_eq!(selection.included.len(), 1);
        assert_eq!(selection.excluded.len(), 2);
        assert_eq!(selection.included[0].targets, ["/config"]);
    }

    #[test]
    fn named_volumes_are_portable_bundle_state() {
        let config = json!({
            "name": "root",
            "services": {
                "database": {"volumes": [
                    {"type":"volume", "source":"database-data", "target":"/var/lib/data"}
                ]}
            },
            "volumes": {
                "database-data": {"name":"root_database-data"}
            }
        });
        let selection =
            select_backup_mounts(&config, Path::new("/__bundle_backup_sentinel__")).unwrap();
        assert_eq!(selection.included.len(), 1);
        assert_eq!(selection.included[0].mount_type, "volume");
        assert_eq!(selection.included[0].source, "database-data");
        assert_eq!(selection.included[0].targets, ["/var/lib/data"]);
        assert_eq!(
            compose_volume_name(&config, "database-data").unwrap(),
            "root_database-data"
        );
    }

    #[test]
    fn archive_paths_reject_traversal() {
        assert!(ensure_safe_archive_path(Path::new("mounts/0001/data")).is_ok());
        assert!(ensure_safe_archive_path(Path::new("../etc/shadow")).is_err());
        assert!(ensure_safe_archive_path(Path::new("/etc/shadow")).is_err());
        assert!(
            ensure_safe_symlink(Path::new("mysql.sock"), Path::new("/run/mysql.sock")).is_err()
        );
    }

    #[test]
    fn sparse_database_files_are_supported_but_devices_are_not() {
        assert!(supported_entry_type(EntryType::GNUSparse));
        assert!(!supported_entry_type(EntryType::Char));
    }

    #[test]
    fn restore_destinations_stay_under_nfs() {
        let backup = Path::new("/nfs/jellyfin/.docker-services-backups");
        assert!(ensure_safe_restore_destination(Path::new("/etc"), backup).is_err());
        assert!(ensure_safe_restore_destination(backup, backup).is_err());
    }

    #[test]
    fn ids_are_path_safe() {
        assert!(validate_id("20260731T120000Z-1234abcd").is_ok());
        assert!(validate_id("../../shadow").is_err());
        assert!(validate_id("with space").is_err());
    }

    #[test]
    fn valid_setup_bundle_is_inspected_without_extracting() {
        let directory = tempdir().unwrap();
        let bundle = directory.path().join("setup.tar.gz");
        let manifest = SetupManifest {
            schema_version: SCHEMA_VERSION,
            application: "docker-services".into(),
            application_version: "test".into(),
            created_at: "2026-07-31T12:00:00Z".into(),
            host: "192.0.2.10".into(),
            project: Some("test".into()),
            kind: "manual".into(),
            consistency: "online".into(),
            control_files: vec![ControlFile {
                kind: "compose".into(),
                original_path: "/root/docker-compose.yml".into(),
                archive_path: "control/docker-compose.yml".into(),
                present: true,
            }],
            services: vec![ServiceIntent {
                name: "app".into(),
                disabled: false,
            }],
            mounts: Vec::new(),
            excluded_mounts: Vec::new(),
            excluded_paths: Vec::new(),
        };
        let file = File::create(&bundle).unwrap();
        let encoder = GzEncoder::new(file, Compression::fast());
        let mut archive = Builder::new(encoder);
        append_bytes(
            &mut archive,
            MANIFEST_PATH,
            &serde_json::to_vec(&manifest).unwrap(),
            0o600,
        )
        .unwrap();
        append_bytes(
            &mut archive,
            "control/docker-compose.yml",
            b"services:\n  app:\n    image: example/app\n",
            0o600,
        )
        .unwrap();
        archive.into_inner().unwrap().finish().unwrap();

        let store = BackupStore::new(BackupPolicy::new(
            directory.path().join("backups"),
            Duration::from_secs(3600),
            4,
        ));
        let inspection = store.inspect(&bundle).unwrap();
        assert_eq!(inspection.manifest, manifest);
        assert_eq!(inspection.file_name, "setup.tar.gz");
        assert_eq!(inspection.sha256.len(), 64);
    }

    #[test]
    fn legacy_mounts_default_ownership_but_partial_metadata_is_rejected() {
        let mut manifest: SetupManifest = serde_json::from_value(json!({
            "schema_version": 2,
            "application": "docker-services",
            "application_version": "test",
            "created_at": "2026-07-31T12:00:00Z",
            "host": "192.0.2.10",
            "project": "test",
            "kind": "manual",
            "consistency": "online",
            "control_files": [{
                "kind": "compose",
                "original_path": "/root/docker-compose.yml",
                "archive_path": "control/docker-compose.yml",
                "present": true
            }],
            "services": [],
            "mounts": [{
                "id": "0001",
                "mount_type": "bind",
                "source": "/nfs/app/config",
                "services": ["app"],
                "targets": ["/config"],
                "source_type": "directory",
                "archive_path": "mounts/0001/data",
                "present": true
            }],
            "excluded_mounts": []
        }))
        .unwrap();
        assert!(manifest.mounts[0].owner_uid.is_none());
        assert!(manifest.excluded_paths.is_empty());
        assert!(validate_manifest(&manifest).is_ok());

        manifest.mounts[0].owner_uid = Some(1000);
        assert!(validate_manifest(&manifest).is_err());
    }
}
