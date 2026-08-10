use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use magi_core::{UtcMillis, fs_atomic::write_atomic};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BrowserHostProtocolRange, BrowserHostProtocolVersion};

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub const BROWSER_RUNTIME_MANIFEST_FORMAT_VERSION: u16 = 1;
pub const BROWSER_RUNTIME_MANIFEST_FILE: &str = "manifest.json";
pub const BROWSER_RUNTIME_RELEASE_FILE: &str = "release.json";
pub const BROWSER_RUNTIME_ACTIVE_FILE: &str = "active.json";
pub const BROWSER_RUNTIME_TRUST_FILE: &str = "trust.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserRuntimeReleaseChannel {
    Stable,
    Beta,
    Nightly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserRuntimeUpdateLevel {
    Optional,
    Recommended,
    RequiredSecurity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeTarget {
    pub os: String,
    pub arch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeFile {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub executable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeManifest {
    pub format_version: u16,
    pub runtime_version: Version,
    pub host_version: Version,
    pub host_protocol: BrowserHostProtocolRange,
    pub node_version: Version,
    pub playwright_version: Version,
    pub chromium_version: String,
    pub target: BrowserRuntimeTarget,
    pub channel: BrowserRuntimeReleaseChannel,
    pub manifest_sequence: u64,
    pub released_at: UtcMillis,
    pub expires_at: UtcMillis,
    pub minimum_magi_version: Version,
    pub minimum_safe_runtime_version: Version,
    pub unpacked_size_bytes: u64,
    pub node_executable_path: String,
    pub host_entry_path: String,
    pub chromium_executable_path: String,
    pub files: Vec<BrowserRuntimeFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserRuntimeEntrypoints {
    pub install_root: PathBuf,
    pub node_executable: PathBuf,
    pub host_entry: PathBuf,
    pub chromium_executable: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedBrowserRuntimeRelease {
    pub manifest: BrowserRuntimeManifest,
    pub update_level: BrowserRuntimeUpdateLevel,
    pub archive_sha256: String,
    pub archive_size_bytes: u64,
    pub signature: String,
}

impl SignedBrowserRuntimeRelease {
    pub fn signing_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&BrowserRuntimeReleaseSigningPayload {
            manifest: &self.manifest,
            update_level: self.update_level,
            archive_sha256: &self.archive_sha256,
            archive_size_bytes: self.archive_size_bytes,
        })
    }
}

#[derive(Serialize)]
struct BrowserRuntimeReleaseSigningPayload<'a> {
    manifest: &'a BrowserRuntimeManifest,
    update_level: BrowserRuntimeUpdateLevel,
    archive_sha256: &'a str,
    archive_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveBrowserRuntime {
    pub runtime_version: Version,
    pub manifest_sequence: u64,
    pub activated_at: UtcMillis,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeTrustState {
    pub highest_manifest_sequence: u64,
    pub minimum_safe_runtime_version: Option<Version>,
}

#[derive(Clone, Debug)]
pub struct BrowserRuntimeManagerConfig {
    pub root: PathBuf,
    pub target: BrowserRuntimeTarget,
    pub channel: BrowserRuntimeReleaseChannel,
    pub magi_version: Version,
    pub host_protocol_version: BrowserHostProtocolVersion,
    pub trusted_release_key: [u8; 32],
    pub max_archive_size_bytes: u64,
    pub max_unpacked_size_bytes: u64,
}

impl BrowserRuntimeManagerConfig {
    pub fn production_defaults(
        root: PathBuf,
        target: BrowserRuntimeTarget,
        channel: BrowserRuntimeReleaseChannel,
        magi_version: Version,
        trusted_release_key: [u8; 32],
    ) -> Self {
        Self {
            root,
            target,
            channel,
            magi_version,
            host_protocol_version: BrowserHostProtocolVersion::CURRENT,
            trusted_release_key,
            max_archive_size_bytes: 512 * 1024 * 1024,
            max_unpacked_size_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserRuntimeReleaseAssessment {
    pub runtime_version: Version,
    pub minimum_magi_version: Version,
    pub magi_update_required: bool,
    pub update_level: BrowserRuntimeUpdateLevel,
    pub archive_size_bytes: u64,
    pub requires_install: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserRuntimeInstallOutcome {
    pub active: ActiveBrowserRuntime,
    pub install_path: PathBuf,
}

pub trait BrowserRuntimeSelfTest {
    fn run(&self, install_root: &Path, manifest: &BrowserRuntimeManifest) -> Result<(), String>;
}

impl<F> BrowserRuntimeSelfTest for F
where
    F: Fn(&Path, &BrowserRuntimeManifest) -> Result<(), String>,
{
    fn run(&self, install_root: &Path, manifest: &BrowserRuntimeManifest) -> Result<(), String> {
        self(install_root, manifest)
    }
}

#[derive(Debug)]
pub struct BrowserRuntimeManager {
    config: BrowserRuntimeManagerConfig,
}

impl BrowserRuntimeManager {
    pub fn new(config: BrowserRuntimeManagerConfig) -> Self {
        Self { config }
    }

    pub fn root(&self) -> &Path {
        &self.config.root
    }

    pub fn runtime_path(&self, version: &Version) -> PathBuf {
        self.config.root.join(version.to_string())
    }

    pub fn active(&self) -> Result<Option<ActiveBrowserRuntime>, BrowserRuntimeComponentError> {
        read_optional_json(&self.config.root.join(BROWSER_RUNTIME_ACTIVE_FILE))
    }

    pub fn trust_state(&self) -> Result<BrowserRuntimeTrustState, BrowserRuntimeComponentError> {
        Ok(
            read_optional_json(&self.config.root.join(BROWSER_RUNTIME_TRUST_FILE))?
                .unwrap_or_default(),
        )
    }

    pub fn assess_release(
        &self,
        release: &SignedBrowserRuntimeRelease,
        now: UtcMillis,
    ) -> Result<BrowserRuntimeReleaseAssessment, BrowserRuntimeComponentError> {
        let trust = self.trust_state()?;
        self.verify_release(release, &trust, now, true, false)?;
        let active = self.active()?;
        Ok(BrowserRuntimeReleaseAssessment {
            runtime_version: release.manifest.runtime_version.clone(),
            minimum_magi_version: release.manifest.minimum_magi_version.clone(),
            magi_update_required: self.config.magi_version < release.manifest.minimum_magi_version,
            update_level: release.update_level,
            archive_size_bytes: release.archive_size_bytes,
            requires_install: active.as_ref().is_none_or(|active| {
                active.runtime_version != release.manifest.runtime_version
                    || active.manifest_sequence != release.manifest.manifest_sequence
                    || self.inspect_active_release(now).map_or(true, |installed| {
                        installed
                            .as_ref()
                            .is_none_or(|installed| !same_release_identity(installed, release))
                    })
            }),
        })
    }

    pub fn install_archive(
        &self,
        release: &SignedBrowserRuntimeRelease,
        archive_path: &Path,
        now: UtcMillis,
        self_test: &dyn BrowserRuntimeSelfTest,
    ) -> Result<BrowserRuntimeInstallOutcome, BrowserRuntimeComponentError> {
        fs::create_dir_all(&self.config.root)?;
        let trust = self.trust_state()?;
        self.verify_release(release, &trust, now, true, true)?;
        verify_archive(archive_path, release, self.config.max_archive_size_bytes)?;

        let staging_root = self.config.root.join(".staging");
        fs::create_dir_all(&staging_root)?;
        let staging_path = staging_root.join(format!(
            "{}-{}-{}",
            release.manifest.runtime_version,
            std::process::id(),
            STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&staging_path)?;
        let mut cleanup = StagingCleanup::new(staging_path.clone());

        extract_runtime_archive(
            archive_path,
            &staging_path,
            &release.manifest,
            self.config.max_unpacked_size_bytes,
        )?;
        self_test
            .run(&staging_path, &release.manifest)
            .map_err(BrowserRuntimeComponentError::SelfTestFailed)?;
        write_json(&staging_path.join(BROWSER_RUNTIME_RELEASE_FILE), release)?;

        let install_path = self.runtime_path(&release.manifest.runtime_version);
        if install_path.exists() {
            if verify_existing_install(&install_path, release).is_ok() {
                fs::remove_dir_all(&staging_path)?;
            } else {
                replace_runtime_install(&self.config.root, &staging_path, &install_path)?;
            }
        } else {
            fs::rename(&staging_path, &install_path)?;
            sync_directory(&self.config.root);
        }
        cleanup.disarm();

        let next_trust = BrowserRuntimeTrustState {
            highest_manifest_sequence: trust
                .highest_manifest_sequence
                .max(release.manifest.manifest_sequence),
            minimum_safe_runtime_version: max_version(
                trust.minimum_safe_runtime_version,
                Some(release.manifest.minimum_safe_runtime_version.clone()),
            ),
        };
        write_json(
            &self.config.root.join(BROWSER_RUNTIME_TRUST_FILE),
            &next_trust,
        )?;
        let active = ActiveBrowserRuntime {
            runtime_version: release.manifest.runtime_version.clone(),
            manifest_sequence: release.manifest.manifest_sequence,
            activated_at: now,
        };
        write_json(&self.config.root.join(BROWSER_RUNTIME_ACTIVE_FILE), &active)?;
        Ok(BrowserRuntimeInstallOutcome {
            active,
            install_path,
        })
    }

    pub fn inspect_active_release(
        &self,
        now: UtcMillis,
    ) -> Result<Option<SignedBrowserRuntimeRelease>, BrowserRuntimeComponentError> {
        let Some(active) = self.active()? else {
            return Ok(None);
        };
        let install_path = self.runtime_path(&active.runtime_version);
        let release: SignedBrowserRuntimeRelease =
            read_required_json(&install_path.join(BROWSER_RUNTIME_RELEASE_FILE))?;
        let trust = self.trust_state()?;
        self.verify_release(&release, &trust, now, false, true)?;
        verify_installed_files(&install_path, &release.manifest)?;
        if release.manifest.manifest_sequence != active.manifest_sequence {
            return Err(BrowserRuntimeComponentError::ActiveStateMismatch);
        }
        Ok(Some(release))
    }

    pub fn active_entrypoints(
        &self,
        now: UtcMillis,
    ) -> Result<Option<BrowserRuntimeEntrypoints>, BrowserRuntimeComponentError> {
        let Some(release) = self.inspect_active_release(now)? else {
            return Ok(None);
        };
        let install_root = self.runtime_path(&release.manifest.runtime_version);
        Ok(Some(BrowserRuntimeEntrypoints {
            node_executable: install_root.join(&release.manifest.node_executable_path),
            host_entry: install_root.join(&release.manifest.host_entry_path),
            chromium_executable: install_root.join(&release.manifest.chromium_executable_path),
            install_root,
        }))
    }

    pub fn uninstall(&self) -> Result<bool, BrowserRuntimeComponentError> {
        let active = self.active()?;
        if active.is_none() && !self.config.root.exists() {
            return Ok(false);
        }
        let active_path = self.config.root.join(BROWSER_RUNTIME_ACTIVE_FILE);
        match fs::remove_file(&active_path) {
            Ok(()) => sync_directory(&self.config.root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if self.config.root.exists() {
            for entry in fs::read_dir(&self.config.root)? {
                let entry = entry?;
                let path = entry.path();
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                fs::remove_dir_all(path)?;
            }
            sync_directory(&self.config.root);
        }
        Ok(active.is_some())
    }

    fn verify_release(
        &self,
        release: &SignedBrowserRuntimeRelease,
        trust: &BrowserRuntimeTrustState,
        now: UtcMillis,
        enforce_expiry: bool,
        enforce_magi_version: bool,
    ) -> Result<(), BrowserRuntimeComponentError> {
        let manifest = &release.manifest;
        if manifest.format_version != BROWSER_RUNTIME_MANIFEST_FORMAT_VERSION {
            return Err(BrowserRuntimeComponentError::UnsupportedManifestFormat(
                manifest.format_version,
            ));
        }
        if manifest.target != self.config.target {
            return Err(BrowserRuntimeComponentError::TargetMismatch);
        }
        if manifest.channel != self.config.channel {
            return Err(BrowserRuntimeComponentError::ChannelMismatch);
        }
        if enforce_expiry && now > manifest.expires_at {
            return Err(BrowserRuntimeComponentError::ManifestExpired);
        }
        if manifest.released_at > manifest.expires_at {
            return Err(BrowserRuntimeComponentError::InvalidManifest(
                "released_at is later than expires_at".to_string(),
            ));
        }
        if manifest.manifest_sequence < trust.highest_manifest_sequence {
            return Err(BrowserRuntimeComponentError::ManifestReplay {
                accepted: trust.highest_manifest_sequence,
                received: manifest.manifest_sequence,
            });
        }
        let magi_version_supported = self.config.magi_version >= manifest.minimum_magi_version;
        if enforce_magi_version && !magi_version_supported {
            return Err(BrowserRuntimeComponentError::MagiVersionTooOld {
                minimum: manifest.minimum_magi_version.clone(),
                current: self.config.magi_version.clone(),
            });
        }
        if !manifest.host_protocol.is_valid() {
            return Err(BrowserRuntimeComponentError::HostProtocolIncompatible);
        }
        // 更新检查必须先能识别“需要升级 Magi”。当清单明确要求更高版本 Magi 时，
        // 当前进程的 Host 协议不兼容是预期结果；安装阶段仍由版本门禁先行拒绝。
        if magi_version_supported
            && !manifest
                .host_protocol
                .contains(self.config.host_protocol_version)
        {
            return Err(BrowserRuntimeComponentError::HostProtocolIncompatible);
        }
        let minimum_safe = max_version(
            trust.minimum_safe_runtime_version.clone(),
            Some(manifest.minimum_safe_runtime_version.clone()),
        )
        .expect("minimum safe runtime is always present");
        if manifest.runtime_version < minimum_safe {
            return Err(BrowserRuntimeComponentError::RuntimeBelowMinimumSafe {
                minimum: minimum_safe,
                received: manifest.runtime_version.clone(),
            });
        }
        if release.archive_size_bytes > self.config.max_archive_size_bytes {
            return Err(BrowserRuntimeComponentError::ArchiveTooLarge {
                maximum: self.config.max_archive_size_bytes,
                received: release.archive_size_bytes,
            });
        }
        if manifest.unpacked_size_bytes > self.config.max_unpacked_size_bytes {
            return Err(BrowserRuntimeComponentError::UnpackedRuntimeTooLarge {
                maximum: self.config.max_unpacked_size_bytes,
                received: manifest.unpacked_size_bytes,
            });
        }
        validate_manifest_files(manifest)?;
        verify_sha256_text(&release.archive_sha256)?;
        let verifying_key = VerifyingKey::from_bytes(&self.config.trusted_release_key)
            .map_err(|_| BrowserRuntimeComponentError::InvalidReleaseKey)?;
        let signature_bytes = BASE64_STANDARD
            .decode(&release.signature)
            .map_err(|_| BrowserRuntimeComponentError::InvalidSignatureEncoding)?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| BrowserRuntimeComponentError::InvalidSignatureEncoding)?;
        let signing_bytes = release.signing_bytes()?;
        verifying_key
            .verify(&signing_bytes, &signature)
            .map_err(|_| BrowserRuntimeComponentError::InvalidSignature)
    }
}

fn validate_manifest_files(
    manifest: &BrowserRuntimeManifest,
) -> Result<(), BrowserRuntimeComponentError> {
    if manifest.files.is_empty() {
        return Err(BrowserRuntimeComponentError::InvalidManifest(
            "runtime file list is empty".to_string(),
        ));
    }
    let mut previous = None::<&str>;
    let mut total = 0u64;
    for file in &manifest.files {
        validate_relative_path(Path::new(&file.path))?;
        if let Some(target) = file.symlink_target.as_deref() {
            validate_symlink_target(Path::new(&file.path), Path::new(target))?;
            if file.size_bytes != 0 || !file.sha256.is_empty() || file.executable {
                return Err(BrowserRuntimeComponentError::InvalidManifest(format!(
                    "symlink metadata must not declare size, hash, or executable: {}",
                    file.path
                )));
            }
        } else {
            verify_sha256_text(&file.sha256)?;
        }
        if file.path == BROWSER_RUNTIME_MANIFEST_FILE || file.path == BROWSER_RUNTIME_RELEASE_FILE {
            return Err(BrowserRuntimeComponentError::InvalidManifest(format!(
                "reserved metadata path appears in runtime file list: {}",
                file.path
            )));
        }
        if previous.is_some_and(|previous| previous >= file.path.as_str()) {
            return Err(BrowserRuntimeComponentError::InvalidManifest(
                "runtime file list must be strictly sorted and unique".to_string(),
            ));
        }
        previous = Some(&file.path);
        total = total
            .checked_add(file.size_bytes)
            .ok_or_else(|| BrowserRuntimeComponentError::InvalidManifest("size overflow".into()))?;
    }
    if total != manifest.unpacked_size_bytes {
        return Err(BrowserRuntimeComponentError::InvalidManifest(format!(
            "unpacked size does not match file list: manifest={}, files={total}",
            manifest.unpacked_size_bytes
        )));
    }
    validate_manifest_entrypoint(
        manifest,
        &manifest.node_executable_path,
        "node_executable_path",
        true,
    )?;
    validate_manifest_entrypoint(
        manifest,
        &manifest.host_entry_path,
        "host_entry_path",
        false,
    )?;
    validate_manifest_entrypoint(
        manifest,
        &manifest.chromium_executable_path,
        "chromium_executable_path",
        true,
    )?;
    Ok(())
}

fn validate_manifest_entrypoint(
    manifest: &BrowserRuntimeManifest,
    path: &str,
    field: &str,
    must_be_executable: bool,
) -> Result<(), BrowserRuntimeComponentError> {
    validate_relative_path(Path::new(path))?;
    let file = manifest
        .files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| {
            BrowserRuntimeComponentError::InvalidManifest(format!(
                "{field} does not reference a runtime file: {path}"
            ))
        })?;
    if must_be_executable && (!file.executable || file.symlink_target.is_some()) {
        return Err(BrowserRuntimeComponentError::InvalidManifest(format!(
            "{field} must reference an executable runtime file: {path}"
        )));
    }
    Ok(())
}

fn verify_archive(
    archive_path: &Path,
    release: &SignedBrowserRuntimeRelease,
    maximum_size: u64,
) -> Result<(), BrowserRuntimeComponentError> {
    let metadata = fs::metadata(archive_path)?;
    if metadata.len() != release.archive_size_bytes {
        return Err(BrowserRuntimeComponentError::ArchiveSizeMismatch {
            expected: release.archive_size_bytes,
            actual: metadata.len(),
        });
    }
    if metadata.len() > maximum_size {
        return Err(BrowserRuntimeComponentError::ArchiveTooLarge {
            maximum: maximum_size,
            received: metadata.len(),
        });
    }
    let actual = sha256_file(archive_path)?;
    if actual != release.archive_sha256 {
        return Err(BrowserRuntimeComponentError::ArchiveHashMismatch);
    }
    Ok(())
}

fn extract_runtime_archive(
    archive_path: &Path,
    destination: &Path,
    manifest: &BrowserRuntimeManifest,
    maximum_unpacked_size: u64,
) -> Result<(), BrowserRuntimeComponentError> {
    let archive_file = File::open(archive_path)?;
    let decoder = zstd::stream::read::Decoder::new(archive_file)?;
    let mut archive = tar::Archive::new(decoder);
    let mut extracted_files = HashSet::new();
    let mut extracted_size = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        validate_relative_path(&relative)?;
        let entry_type = entry.header().entry_type();
        let output_path = destination.join(&relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&output_path)?;
            continue;
        }
        let relative_text = path_to_manifest_text(&relative)?;
        let expected = manifest
            .files
            .iter()
            .find(|file| file.path == relative_text);
        if expected.is_none() && relative_text != BROWSER_RUNTIME_MANIFEST_FILE {
            return Err(BrowserRuntimeComponentError::ArchiveFileSetMismatch {
                missing: Vec::new(),
                unexpected: vec![relative_text.clone()],
            });
        }
        if entry_type.is_symlink() {
            let expected = expected.ok_or_else(|| {
                BrowserRuntimeComponentError::UnsupportedArchiveEntry(relative_text.clone())
            })?;
            let target = entry
                .link_name()?
                .ok_or_else(|| {
                    BrowserRuntimeComponentError::InvalidManifest(format!(
                        "symlink target is missing: {relative_text}"
                    ))
                })?
                .into_owned();
            let target_text = path_to_symlink_text(&target)?;
            validate_symlink_target(&relative, &target)?;
            if expected.symlink_target.as_deref() != Some(target_text.as_str()) {
                return Err(BrowserRuntimeComponentError::InvalidManifest(format!(
                    "symlink target does not match manifest: {relative_text}"
                )));
            }
            if !extracted_files.insert(relative_text.clone()) {
                return Err(BrowserRuntimeComponentError::DuplicateArchiveEntry(
                    relative_text,
                ));
            }
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)?;
            }
            create_symlink(&target, &output_path)?;
            continue;
        }
        if !entry_type.is_file() || expected.is_some_and(|file| file.symlink_target.is_some()) {
            return Err(BrowserRuntimeComponentError::UnsupportedArchiveEntry(
                relative.display().to_string(),
            ));
        }
        if !extracted_files.insert(relative_text.clone()) {
            return Err(BrowserRuntimeComponentError::DuplicateArchiveEntry(
                relative_text,
            ));
        }
        extracted_size = extracted_size.checked_add(entry.size()).ok_or(
            BrowserRuntimeComponentError::UnpackedRuntimeTooLarge {
                maximum: maximum_unpacked_size,
                received: u64::MAX,
            },
        )?;
        if extracted_size > maximum_unpacked_size {
            return Err(BrowserRuntimeComponentError::UnpackedRuntimeTooLarge {
                maximum: maximum_unpacked_size,
                received: extracted_size,
            });
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&output_path)?;
    }
    let expected_files = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .chain(std::iter::once(BROWSER_RUNTIME_MANIFEST_FILE.to_string()))
        .collect::<HashSet<_>>();
    if extracted_files != expected_files {
        let mut missing = expected_files
            .difference(&extracted_files)
            .cloned()
            .collect::<Vec<_>>();
        let mut unexpected = extracted_files
            .difference(&expected_files)
            .cloned()
            .collect::<Vec<_>>();
        missing.sort();
        unexpected.sort();
        return Err(BrowserRuntimeComponentError::ArchiveFileSetMismatch {
            missing,
            unexpected,
        });
    }
    let embedded_manifest: BrowserRuntimeManifest =
        read_required_json(&destination.join(BROWSER_RUNTIME_MANIFEST_FILE))?;
    if &embedded_manifest != manifest {
        return Err(BrowserRuntimeComponentError::EmbeddedManifestMismatch);
    }
    verify_installed_files(destination, manifest)
}

fn verify_installed_files(
    root: &Path,
    manifest: &BrowserRuntimeManifest,
) -> Result<(), BrowserRuntimeComponentError> {
    for file in &manifest.files {
        let path = root.join(&file.path);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            BrowserRuntimeComponentError::InstalledFileInvalid {
                path: file.path.clone(),
                reason: error.to_string(),
            }
        })?;
        if let Some(expected_target) = file.symlink_target.as_deref() {
            if !metadata.file_type().is_symlink() {
                return Err(BrowserRuntimeComponentError::InstalledFileInvalid {
                    path: file.path.clone(),
                    reason: "expected a symbolic link".to_string(),
                });
            }
            let actual_target = fs::read_link(&path)?;
            if path_to_symlink_text(&actual_target)? != expected_target {
                return Err(BrowserRuntimeComponentError::InstalledFileInvalid {
                    path: file.path.clone(),
                    reason: "symbolic link target mismatch".to_string(),
                });
            }
            continue;
        }
        if !metadata.is_file() || metadata.len() != file.size_bytes {
            return Err(BrowserRuntimeComponentError::InstalledFileInvalid {
                path: file.path.clone(),
                reason: "file type or size mismatch".to_string(),
            });
        }
        if sha256_file(&path)? != file.sha256 {
            return Err(BrowserRuntimeComponentError::InstalledFileInvalid {
                path: file.path.clone(),
                reason: "SHA-256 mismatch".to_string(),
            });
        }
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(BrowserRuntimeComponentError::InstalledFileInvalid {
                    path: file.path.clone(),
                    reason: "executable bit is missing".to_string(),
                });
            }
        }
    }
    Ok(())
}

fn verify_existing_install(
    install_path: &Path,
    release: &SignedBrowserRuntimeRelease,
) -> Result<(), BrowserRuntimeComponentError> {
    let installed: SignedBrowserRuntimeRelease =
        read_required_json(&install_path.join(BROWSER_RUNTIME_RELEASE_FILE))?;
    if !same_release_identity(&installed, release) {
        return Err(BrowserRuntimeComponentError::InvalidManifest(
            "installed runtime release does not match the requested release".to_string(),
        ));
    }
    verify_installed_files(install_path, &release.manifest)
}

fn same_release_identity(
    installed: &SignedBrowserRuntimeRelease,
    requested: &SignedBrowserRuntimeRelease,
) -> bool {
    installed.manifest.manifest_sequence == requested.manifest.manifest_sequence
        && installed.archive_sha256 == requested.archive_sha256
        && installed.archive_size_bytes == requested.archive_size_bytes
        && installed.signature == requested.signature
}

fn replace_runtime_install(
    root: &Path,
    staging_path: &Path,
    install_path: &Path,
) -> Result<(), BrowserRuntimeComponentError> {
    let backup_path = root.join(format!(
        ".replacing-{}-{}",
        install_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtime"),
        STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::rename(install_path, &backup_path)?;
    if let Err(error) = fs::rename(staging_path, install_path) {
        let _ = fs::rename(&backup_path, install_path);
        return Err(error.into());
    }
    let _ = fs::remove_dir_all(&backup_path);
    sync_directory(root);
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), BrowserRuntimeComponentError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BrowserRuntimeComponentError::UnsafeArchivePath(
            path.display().to_string(),
        ));
    }
    Ok(())
}

fn path_to_manifest_text(path: &Path) -> Result<String, BrowserRuntimeComponentError> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| BrowserRuntimeComponentError::NonUtf8ArchivePath),
            _ => Err(BrowserRuntimeComponentError::UnsafeArchivePath(
                path.display().to_string(),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts.join("/"))
}

fn path_to_symlink_text(path: &Path) -> Result<String, BrowserRuntimeComponentError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(BrowserRuntimeComponentError::UnsafeArchivePath(
            path.display().to_string(),
        ));
    }
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or(BrowserRuntimeComponentError::NonUtf8ArchivePath),
            Component::CurDir => Ok(".".to_string()),
            Component::ParentDir => Ok("..".to_string()),
            Component::RootDir | Component::Prefix(_) => Err(
                BrowserRuntimeComponentError::UnsafeArchivePath(path.display().to_string()),
            ),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts.join("/"))
}

fn validate_symlink_target(
    link_path: &Path,
    target: &Path,
) -> Result<(), BrowserRuntimeComponentError> {
    if target.as_os_str().is_empty() || target.is_absolute() {
        return Err(BrowserRuntimeComponentError::UnsafeArchivePath(
            target.display().to_string(),
        ));
    }
    let mut depth = link_path
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        match component {
            Component::Normal(value) => {
                value
                    .to_str()
                    .ok_or(BrowserRuntimeComponentError::NonUtf8ArchivePath)?;
                depth = depth.saturating_add(1);
            }
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(BrowserRuntimeComponentError::UnsafeArchivePath(
                    target.display().to_string(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, output: &Path) -> Result<(), BrowserRuntimeComponentError> {
    std::os::unix::fs::symlink(target, output)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, output: &Path) -> Result<(), BrowserRuntimeComponentError> {
    Err(BrowserRuntimeComponentError::UnsupportedArchiveEntry(
        output.display().to_string(),
    ))
}

fn verify_sha256_text(value: &str) -> Result<(), BrowserRuntimeComponentError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BrowserRuntimeComponentError::InvalidSha256(
            value.to_string(),
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, BrowserRuntimeComponentError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, BrowserRuntimeComponentError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_required_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<T, BrowserRuntimeComponentError> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), BrowserRuntimeComponentError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_atomic(path, bytes)?;
    Ok(())
}

fn max_version(first: Option<Version>, second: Option<Version>) -> Option<Version> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.max(second)),
        (Some(first), None) => Some(first),
        (None, Some(second)) => Some(second),
        (None, None) => None,
    }
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

struct StagingCleanup {
    path: PathBuf,
    armed: bool,
}

impl StagingCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrowserRuntimeComponentError {
    #[error("browser runtime I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("browser runtime JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported browser runtime manifest format: {0}")]
    UnsupportedManifestFormat(u16),
    #[error("browser runtime target does not match this Magi build")]
    TargetMismatch,
    #[error("browser runtime release channel does not match")]
    ChannelMismatch,
    #[error("browser runtime manifest has expired")]
    ManifestExpired,
    #[error("browser runtime manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("browser runtime manifest replay: accepted={accepted}, received={received}")]
    ManifestReplay { accepted: u64, received: u64 },
    #[error("Magi version is too old for browser runtime: current={current}, minimum={minimum}")]
    MagiVersionTooOld { minimum: Version, current: Version },
    #[error("browser Host protocol is incompatible")]
    HostProtocolIncompatible,
    #[error(
        "browser runtime is below minimum safe version: received={received}, minimum={minimum}"
    )]
    RuntimeBelowMinimumSafe { minimum: Version, received: Version },
    #[error("browser runtime archive is too large: received={received}, maximum={maximum}")]
    ArchiveTooLarge { maximum: u64, received: u64 },
    #[error("browser runtime unpacked size is too large: received={received}, maximum={maximum}")]
    UnpackedRuntimeTooLarge { maximum: u64, received: u64 },
    #[error("browser runtime release key is invalid")]
    InvalidReleaseKey,
    #[error("browser runtime signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("browser runtime signature is invalid")]
    InvalidSignature,
    #[error("invalid SHA-256 value: {0}")]
    InvalidSha256(String),
    #[error("browser runtime archive size mismatch: expected={expected}, actual={actual}")]
    ArchiveSizeMismatch { expected: u64, actual: u64 },
    #[error("browser runtime archive SHA-256 mismatch")]
    ArchiveHashMismatch,
    #[error("unsafe browser runtime archive path: {0}")]
    UnsafeArchivePath(String),
    #[error("browser runtime archive path is not UTF-8")]
    NonUtf8ArchivePath,
    #[error("unsupported browser runtime archive entry: {0}")]
    UnsupportedArchiveEntry(String),
    #[error("duplicate browser runtime archive entry: {0}")]
    DuplicateArchiveEntry(String),
    #[error(
        "browser runtime archive file set mismatch: missing={missing:?}, unexpected={unexpected:?}"
    )]
    ArchiveFileSetMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    #[error("browser runtime embedded manifest does not match signed release")]
    EmbeddedManifestMismatch,
    #[error("browser runtime installed file is invalid: {path}: {reason}")]
    InstalledFileInvalid { path: String, reason: String },
    #[error("browser runtime self-test failed: {0}")]
    SelfTestFailed(String),
    #[error("browser runtime active state does not match installed release")]
    ActiveStateMismatch,
}
