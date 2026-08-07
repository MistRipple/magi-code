use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use magi_browser_runtime::{
    BROWSER_HOST_PROTOCOL_MAJOR, BROWSER_HOST_PROTOCOL_MINOR, BROWSER_RUNTIME_MANIFEST_FILE,
    BROWSER_RUNTIME_MANIFEST_FORMAT_VERSION, BrowserHostProtocolRange, BrowserHostProtocolVersion,
    BrowserRuntimeFile, BrowserRuntimeManager, BrowserRuntimeManagerConfig, BrowserRuntimeManifest,
    BrowserRuntimeReleaseChannel, BrowserRuntimeTarget, BrowserRuntimeUpdateLevel,
    SignedBrowserRuntimeRelease,
};
use magi_core::UtcMillis;
use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = arguments()?;
    let root = canonical_directory(required(&args, "root")?)?;
    let output_dir = PathBuf::from(required(&args, "output-dir")?);
    fs::create_dir_all(&output_dir)?;

    let runtime_version = version(&args, "runtime-version")?;
    let host_version = version(&args, "host-version")?;
    let node_version = version(&args, "node-version")?;
    let playwright_version = version(&args, "playwright-version")?;
    let minimum_magi_version = version(&args, "minimum-magi-version")?;
    let minimum_safe_runtime_version = version(&args, "minimum-safe-runtime-version")?;
    let os = required(&args, "os")?.to_string();
    let arch = required(&args, "arch")?.to_string();
    let manifest_sequence = integer(&args, "manifest-sequence")?;
    let expires_days = integer(&args, "expires-days")?;
    let node_executable_path = required(&args, "node-executable-path")?.to_string();
    let host_entry_path = required(&args, "host-entry-path")?.to_string();
    let chromium_executable_path = required(&args, "chromium-executable-path")?.to_string();
    let archive_url = required(&args, "archive-url")?.to_string();
    let chromium_version = required(&args, "chromium-version")?.to_string();
    let channel = release_channel(required(&args, "channel")?)?;
    let update_level = update_level(required(&args, "update-level")?)?;
    let signing_key = signing_key()?;
    verify_public_key(&args, &signing_key)?;

    let mut files = collect_runtime_files(&root)?;
    for file in &mut files {
        if file.path == node_executable_path || file.path == chromium_executable_path {
            file.executable = true;
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let unpacked_size_bytes = files.iter().map(|file| file.size_bytes).sum();
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let now = u64::try_from(now)?;
    let expires_delta = expires_days
        .checked_mul(86_400_000)
        .ok_or("expires-days overflow")?;
    let manifest = BrowserRuntimeManifest {
        format_version: BROWSER_RUNTIME_MANIFEST_FORMAT_VERSION,
        runtime_version: runtime_version.clone(),
        host_version,
        host_protocol: BrowserHostProtocolRange {
            minimum: BrowserHostProtocolVersion {
                major: BROWSER_HOST_PROTOCOL_MAJOR,
                minor: BROWSER_HOST_PROTOCOL_MINOR,
            },
            maximum: BrowserHostProtocolVersion {
                major: BROWSER_HOST_PROTOCOL_MAJOR,
                minor: BROWSER_HOST_PROTOCOL_MINOR,
            },
        },
        node_version,
        playwright_version,
        chromium_version,
        target: BrowserRuntimeTarget {
            os: os.clone(),
            arch: arch.clone(),
        },
        channel,
        manifest_sequence,
        released_at: UtcMillis(now),
        expires_at: UtcMillis(now.checked_add(expires_delta).ok_or("expiry overflow")?),
        minimum_magi_version: minimum_magi_version.clone(),
        minimum_safe_runtime_version,
        unpacked_size_bytes,
        node_executable_path,
        host_entry_path,
        chromium_executable_path,
        files,
    };
    write_json(&root.join(BROWSER_RUNTIME_MANIFEST_FILE), &manifest)?;

    let archive_name = format!(
        "magi-browser-runtime-{}-{}-{}.tar.zst",
        runtime_version, os, arch
    );
    let archive_path = output_dir.join(&archive_name);
    write_archive(&root, &archive_path, &manifest)?;
    let archive_size_bytes = fs::metadata(&archive_path)?.len();
    let archive_sha256 = sha256_file(&archive_path)?;
    let mut release = SignedBrowserRuntimeRelease {
        manifest,
        update_level,
        archive_sha256,
        archive_size_bytes,
        signature: String::new(),
    };
    release.signature =
        BASE64_STANDARD.encode(signing_key.sign(&release.signing_bytes()?).to_bytes());

    let release_path = output_dir.join(format!("signed-release-{os}-{arch}.json"));
    let feed_path = output_dir.join(format!("release-{os}-{arch}.json"));
    write_json(&release_path, &release)?;
    write_json(
        &feed_path,
        &BrowserRuntimeReleaseFeed {
            release: &release,
            archive_url: &archive_url,
        },
    )?;
    if let Some(verify_root) = args.get("verify-install-root") {
        verify_installed_archive(VerifyInstalledArchive {
            root: Path::new(verify_root),
            archive_path: &archive_path,
            release: &release,
            signing_key: &signing_key,
            os: &os,
            arch: &arch,
            channel,
            magi_version: &minimum_magi_version,
        })?;
    }
    println!(
        "{}",
        serde_json::json!({
            "archive": archive_path,
            "release": release_path,
            "feed": feed_path,
            "publicKeyHex": hex(&signing_key.verifying_key().to_bytes()),
        })
    );
    Ok(())
}

struct VerifyInstalledArchive<'a> {
    root: &'a Path,
    archive_path: &'a Path,
    release: &'a SignedBrowserRuntimeRelease,
    signing_key: &'a SigningKey,
    os: &'a str,
    arch: &'a str,
    channel: BrowserRuntimeReleaseChannel,
    magi_version: &'a Version,
}

fn verify_installed_archive(
    config: VerifyInstalledArchive<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let VerifyInstalledArchive {
        root,
        archive_path,
        release,
        signing_key,
        os,
        arch,
        channel,
        magi_version,
    } = config;
    let _ = fs::remove_dir_all(root);
    let manager = BrowserRuntimeManager::new(BrowserRuntimeManagerConfig::production_defaults(
        root.to_path_buf(),
        BrowserRuntimeTarget {
            os: os.to_string(),
            arch: arch.to_string(),
        },
        channel,
        magi_version.clone(),
        signing_key.verifying_key().to_bytes(),
    ));
    manager.install_archive(
        release,
        archive_path,
        UtcMillis::now(),
        &|install_root: &Path, manifest: &magi_browser_runtime::BrowserRuntimeManifest| {
            for file in &manifest.files {
                let path = install_root.join(&file.path);
                if file.symlink_target.is_some() {
                    if !fs::symlink_metadata(&path)
                        .map_err(|error| error.to_string())?
                        .file_type()
                        .is_symlink()
                    {
                        return Err(format!("runtime symlink self-test failed: {}", file.path));
                    }
                } else if !path.is_file() {
                    return Err(format!("runtime file self-test failed: {}", file.path));
                }
            }
            Ok(())
        },
    )?;
    manager
        .inspect_active_release(UtcMillis::now())?
        .ok_or("runtime archive self-test did not activate an installed release")?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserRuntimeReleaseFeed<'a> {
    release: &'a SignedBrowserRuntimeRelease,
    archive_url: &'a str,
}

fn arguments() -> Result<HashMap<String, String>, String> {
    let values = env::args().skip(1).collect::<Vec<_>>();
    if values.len() % 2 != 0 {
        return Err("arguments must use --name value pairs".to_string());
    }
    let mut parsed = HashMap::new();
    for pair in values.chunks_exact(2) {
        let key = pair[0]
            .strip_prefix("--")
            .ok_or_else(|| format!("invalid argument: {}", pair[0]))?;
        if parsed.insert(key.to_string(), pair[1].clone()).is_some() {
            return Err(format!("duplicate argument: --{key}"));
        }
    }
    Ok(parsed)
}

fn required<'a>(args: &'a HashMap<String, String>, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing --{name}"))
}

fn version(args: &HashMap<String, String>, name: &str) -> Result<Version, String> {
    Version::parse(required(args, name)?).map_err(|error| format!("invalid --{name}: {error}"))
}

fn integer(args: &HashMap<String, String>, name: &str) -> Result<u64, String> {
    required(args, name)?
        .parse()
        .map_err(|error| format!("invalid --{name}: {error}"))
}

fn release_channel(value: &str) -> Result<BrowserRuntimeReleaseChannel, String> {
    match value {
        "stable" => Ok(BrowserRuntimeReleaseChannel::Stable),
        "beta" => Ok(BrowserRuntimeReleaseChannel::Beta),
        "nightly" => Ok(BrowserRuntimeReleaseChannel::Nightly),
        _ => Err(format!("invalid release channel: {value}")),
    }
}

fn update_level(value: &str) -> Result<BrowserRuntimeUpdateLevel, String> {
    match value {
        "optional" => Ok(BrowserRuntimeUpdateLevel::Optional),
        "recommended" => Ok(BrowserRuntimeUpdateLevel::Recommended),
        "required_security" => Ok(BrowserRuntimeUpdateLevel::RequiredSecurity),
        _ => Err(format!("invalid update level: {value}")),
    }
}

fn signing_key() -> Result<SigningKey, String> {
    let value = env::var("MAGI_BROWSER_RUNTIME_SIGNING_KEY_HEX")
        .map_err(|_| "MAGI_BROWSER_RUNTIME_SIGNING_KEY_HEX is required".to_string())?;
    let bytes = parse_hex_32(value.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn verify_public_key(
    args: &HashMap<String, String>,
    signing_key: &SigningKey,
) -> Result<(), String> {
    let Some(expected) = args.get("expected-public-key-hex") else {
        return Ok(());
    };
    let actual = hex(&signing_key.verifying_key().to_bytes());
    if expected.trim().eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err("signing key does not match --expected-public-key-hex".to_string())
    }
}

fn parse_hex_32(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 {
        return Err("signing key must contain 64 hexadecimal characters".to_string());
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "signing key is not hexadecimal".to_string())?;
    }
    Ok(bytes)
}

fn canonical_directory(value: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = PathBuf::from(value).canonicalize()?;
    if !path.is_dir() {
        return Err(format!("runtime root is not a directory: {}", path.display()).into());
    }
    Ok(path)
}

fn collect_runtime_files(
    root: &Path,
) -> Result<Vec<BrowserRuntimeFile>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    collect_directory(root, root, &mut files)?;
    if files.is_empty() {
        return Err("runtime root is empty".into());
    }
    Ok(files)
}

fn collect_directory(
    root: &Path,
    directory: &Path,
    files: &mut Vec<BrowserRuntimeFile>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        let relative_text = manifest_path(relative)?;
        if relative_text == BROWSER_RUNTIME_MANIFEST_FILE {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_directory(root, &path, files)?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path)?;
            files.push(BrowserRuntimeFile {
                path: relative_text,
                sha256: String::new(),
                size_bytes: 0,
                executable: false,
                symlink_target: Some(symlink_path(&target)?),
            });
        } else if metadata.is_file() {
            files.push(BrowserRuntimeFile {
                path: relative_text,
                sha256: sha256_file(&path)?,
                size_bytes: metadata.len(),
                executable: executable(&metadata),
                symlink_target: None,
            });
        } else {
            return Err(format!("unsupported runtime entry: {}", path.display()).into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn manifest_path(path: &Path) -> Result<String, String> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "runtime path is not UTF-8".to_string()),
            _ => Err(format!("unsafe runtime path: {}", path.display())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts.join("/"))
}

fn symlink_path(path: &Path) -> Result<String, String> {
    if path.is_absolute() {
        return Err(format!(
            "absolute symlink target is forbidden: {}",
            path.display()
        ));
    }
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "symlink path is not UTF-8".to_string()),
            Component::CurDir => Ok(".".to_string()),
            Component::ParentDir => Ok("..".to_string()),
            _ => Err(format!("unsafe symlink target: {}", path.display())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parts.join("/"))
}

fn write_archive(
    root: &Path,
    output: &Path,
    manifest: &BrowserRuntimeManifest,
) -> Result<(), Box<dyn std::error::Error>> {
    let archive_file = File::create(output)?;
    let encoder = zstd::stream::write::Encoder::new(archive_file, 12)?;
    let mut archive = tar::Builder::new(encoder);
    archive.follow_symlinks(false);
    for file in &manifest.files {
        archive.append_path_with_name(root.join(&file.path), &file.path)?;
    }
    archive.append_path_with_name(
        root.join(BROWSER_RUNTIME_MANIFEST_FILE),
        BROWSER_RUNTIME_MANIFEST_FILE,
    )?;
    archive.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

fn sha256_file(path: &Path) -> io::Result<String> {
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
