use magi_core::HostPath;
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    fs::Metadata,
    path::{Path, PathBuf},
};

use crate::errors::ApiError;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PathNodeDto {
    pub name: String,
    pub path_ref: String,
    pub display_path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryEntryDto {
    pub name: String,
    pub path_ref: String,
    pub display_path: String,
    pub is_directory: bool,
    pub is_hidden: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryBrowseDto {
    pub path_ref: String,
    pub display_path: String,
    pub parent_path_ref: Option<String>,
    pub breadcrumbs: Vec<PathNodeDto>,
    pub roots: Vec<PathNodeDto>,
    pub entries: Vec<DirectoryEntryDto>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResolvedPathKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedPathDto {
    pub path_ref: String,
    pub display_path: String,
    pub name: String,
    pub kind: ResolvedPathKind,
}

pub(crate) fn resolve_existing_path(
    input: &str,
    base_path_ref: Option<&str>,
) -> Result<PathBuf, ApiError> {
    let base = base_path_ref
        .map(decode_path_ref)
        .transpose()?
        .map(HostPath::into_path_buf);
    let resolved =
        HostPath::resolve_native_input(input, base.as_deref(), dirs::home_dir().as_deref())
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    HostPath::canonicalize(resolved.as_path())
        .map(HostPath::into_path_buf)
        .map_err(|_| ApiError::InvalidInput("路径不可读取或不存在".to_string()))
}

pub(crate) fn decode_path_ref(value: &str) -> Result<HostPath, ApiError> {
    HostPath::from_path_ref(value).map_err(|_| ApiError::InvalidInput("路径引用无效".to_string()))
}

pub(crate) fn resolved_path_dto(path: PathBuf) -> Result<ResolvedPathDto, ApiError> {
    let metadata = std::fs::metadata(&path)
        .map_err(|_| ApiError::InvalidInput("路径不可读取或不存在".to_string()))?;
    let kind = if metadata.is_dir() {
        ResolvedPathKind::Directory
    } else if metadata.is_file() {
        ResolvedPathKind::File
    } else {
        return Err(ApiError::InvalidInput("路径不是文件或目录".to_string()));
    };
    Ok(ResolvedPathDto {
        path_ref: path_ref(&path),
        display_path: display_path(&path),
        name: path_name(&path),
        kind,
    })
}

pub(crate) fn browse_directory(
    path: PathBuf,
    show_hidden: bool,
) -> Result<DirectoryBrowseDto, ApiError> {
    if !path.is_dir() {
        return Err(ApiError::InvalidInput("路径不是目录".to_string()));
    }
    let entries = read_directory_entries(&path, show_hidden)?;
    let parent_path_ref = path.parent().filter(|parent| *parent != path).map(path_ref);
    Ok(DirectoryBrowseDto {
        path_ref: path_ref(&path),
        display_path: display_path(&path),
        parent_path_ref,
        breadcrumbs: breadcrumbs(&path),
        roots: system_roots(),
        entries,
    })
}

fn read_directory_entries(
    path: &Path,
    show_hidden: bool,
) -> Result<Vec<DirectoryEntryDto>, ApiError> {
    struct Entry {
        file_name: OsString,
        display_name: String,
        canonical_path: PathBuf,
        hidden: bool,
    }

    let mut entries = std::fs::read_dir(path)
        .map_err(|_| ApiError::InvalidInput("目录不可读取或不存在".to_string()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if !metadata.is_dir() {
                return None;
            }
            let file_name = entry.file_name();
            let hidden = is_hidden(&file_name, &metadata);
            if hidden && !show_hidden {
                return None;
            }
            let canonical_path = HostPath::canonicalize(entry.path()).ok()?.into_path_buf();
            Some(Entry {
                display_name: file_name.to_string_lossy().into_owned(),
                file_name,
                canonical_path,
                hidden,
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        right
            .hidden
            .cmp(&left.hidden)
            .then_with(|| {
                left.display_name
                    .to_lowercase()
                    .cmp(&right.display_name.to_lowercase())
            })
            .then_with(|| left.file_name.cmp(&right.file_name))
    });

    Ok(entries
        .into_iter()
        .map(|entry| DirectoryEntryDto {
            name: entry.display_name,
            path_ref: path_ref(&entry.canonical_path),
            display_path: display_path(&entry.canonical_path),
            is_directory: true,
            is_hidden: entry.hidden,
        })
        .collect())
}

fn breadcrumbs(path: &Path) -> Vec<PathNodeDto> {
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    ancestors
        .into_iter()
        .map(|ancestor| PathNodeDto {
            name: path_name(ancestor),
            path_ref: path_ref(ancestor),
            display_path: display_path(ancestor),
        })
        .collect()
}

#[cfg(unix)]
fn system_roots() -> Vec<PathNodeDto> {
    let root = Path::new("/");
    vec![PathNodeDto {
        name: "/".to_string(),
        path_ref: path_ref(root),
        display_path: "/".to_string(),
    }]
}

#[cfg(windows)]
fn system_roots() -> Vec<PathNodeDto> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| {
            let drive = format!("{}:\\", (b'A' + index as u8) as char);
            let path = PathBuf::from(&drive);
            PathNodeDto {
                name: drive.clone(),
                path_ref: path_ref(&path),
                display_path: drive,
            }
        })
        .collect()
}

pub(crate) fn path_ref(path: &Path) -> String {
    HostPath::from_path(path.to_path_buf())
        .to_path_ref()
        .as_str()
        .to_string()
}

pub(crate) fn display_path(path: &Path) -> String {
    HostPath::from_path(path.to_path_buf()).display_string()
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| display_path(path))
}

fn is_hidden(file_name: &OsStr, metadata: &Metadata) -> bool {
    let dot_hidden = file_name.to_string_lossy().starts_with('.');
    dot_hidden || platform_hidden(metadata)
}

#[cfg(windows)]
fn platform_hidden(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_attributes() & 0x2 != 0
}

#[cfg(target_os = "macos")]
fn platform_hidden(metadata: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;

    metadata.st_flags() & 0x0000_8000 != 0
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_hidden(_metadata: &Metadata) -> bool {
    false
}
