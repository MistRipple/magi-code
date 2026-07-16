use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
const UNIX_PATH_REF_PREFIX: &str = "mhp1:u:";
#[cfg(windows)]
const WINDOWS_PATH_REF_PREFIX: &str = "mhp1:w:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPath(PathBuf);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPathRef(String);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostPathError {
    #[error("路径引用无效")]
    InvalidPathRef,
    #[error("相对路径缺少基准目录")]
    RelativePathRequiresBase,
    #[error("主目录路径缺少系统主目录")]
    HomeDirectoryUnavailable,
}

impl HostPath {
    pub fn from_path(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<Self> {
        dunce::canonicalize(path).map(Self)
    }

    pub fn display_string(&self) -> String {
        dunce::simplified(&self.0).to_string_lossy().into_owned()
    }

    pub fn to_path_ref(&self) -> HostPathRef {
        HostPathRef(encode_native_path(&self.0))
    }

    pub fn from_path_ref(value: &str) -> Result<Self, HostPathError> {
        decode_native_path(value).map(Self)
    }

    pub fn resolve_native_input(
        input: &str,
        base: Option<&Path>,
        home: Option<&Path>,
    ) -> Result<Self, HostPathError> {
        let trimmed = input.trim();
        if trimmed.starts_with("mhp1:") {
            return Self::from_path_ref(trimmed);
        }
        let path = if trimmed == "~" {
            home.ok_or(HostPathError::HomeDirectoryUnavailable)?
                .to_path_buf()
        } else if let Some(suffix) = trimmed
            .strip_prefix("~/")
            .or_else(|| trimmed.strip_prefix("~\\"))
        {
            home.ok_or(HostPathError::HomeDirectoryUnavailable)?
                .join(suffix)
        } else {
            PathBuf::from(trimmed)
        };

        if path.is_absolute() {
            return Ok(Self(path));
        }

        let base = base.ok_or(HostPathError::RelativePathRequiresBase)?;
        Ok(Self(base.join(path)))
    }
}

impl HostPathRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(unix)]
fn encode_native_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    format!(
        "{UNIX_PATH_REF_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(path.as_os_str().as_bytes())
    )
}

#[cfg(windows)]
fn encode_native_path(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;

    let bytes = path
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    format!("{WINDOWS_PATH_REF_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(unix)]
fn decode_native_path(value: &str) -> Result<PathBuf, HostPathError> {
    use std::os::unix::ffi::OsStringExt;

    let payload = value
        .strip_prefix(UNIX_PATH_REF_PREFIX)
        .ok_or(HostPathError::InvalidPathRef)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| HostPathError::InvalidPathRef)?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(windows)]
fn decode_native_path(value: &str) -> Result<PathBuf, HostPathError> {
    use std::os::windows::ffi::OsStringExt;

    let payload = value
        .strip_prefix(WINDOWS_PATH_REF_PREFIX)
        .ok_or(HostPathError::InvalidPathRef)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| HostPathError::InvalidPathRef)?;
    if bytes.len() % 2 != 0 {
        return Err(HostPathError::InvalidPathRef);
    }
    let wide = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}
