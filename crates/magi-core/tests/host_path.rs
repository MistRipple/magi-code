use magi_core::{HostPath, HostPathError};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[test]
fn host_path_ref_round_trips_utf8_path() {
    let original = std::env::temp_dir().join("magi path").join("目录");
    let encoded = HostPath::from_path(original.clone()).to_path_ref();
    let decoded = HostPath::from_path_ref(encoded.as_str()).expect("path ref should decode");

    assert_eq!(decoded.as_path(), original.as_path());
}

#[cfg(unix)]
#[test]
fn host_path_ref_round_trips_non_utf8_unix_path() {
    use std::os::unix::ffi::OsStringExt;

    let original = PathBuf::from(std::ffi::OsString::from_vec(vec![
        b'/', b't', b'm', b'p', b'/', b'm', b'a', b'g', b'i', b'-', 0xff,
    ]));
    let encoded = HostPath::from_path(original.clone()).to_path_ref();
    let decoded = HostPath::from_path_ref(encoded.as_str()).expect("path ref should decode");

    assert_eq!(decoded.as_path(), original.as_path());
}

#[test]
fn resolve_native_input_uses_explicit_base_for_relative_path() {
    let base = std::env::temp_dir().join("magi-base");
    let resolved =
        HostPath::resolve_native_input("child/folder", Some(&base), Some(Path::new("unused-home")))
            .expect("relative path should resolve");

    assert_eq!(resolved.as_path(), base.join("child/folder"));
}

#[test]
fn resolve_native_input_accepts_path_ref_without_base() {
    let original = std::env::temp_dir().join("magi-path-ref-input");
    let path_ref = HostPath::from_path(original.clone()).to_path_ref();

    let resolved = HostPath::resolve_native_input(path_ref.as_str(), None, None)
        .expect("path ref should resolve without text reconstruction");

    assert_eq!(resolved.as_path(), original.as_path());
}

#[test]
fn resolve_native_input_rejects_relative_path_without_base() {
    let error = HostPath::resolve_native_input("child/folder", None, None)
        .expect_err("relative path without base must fail");

    assert_eq!(error, HostPathError::RelativePathRequiresBase);
}

#[test]
fn resolve_native_input_expands_home_directory() {
    let home = std::env::temp_dir().join("magi-home");
    let resolved =
        HostPath::resolve_native_input("~/project", Some(Path::new("ignored-base")), Some(&home))
            .expect("home path should resolve");

    assert_eq!(resolved.as_path(), home.join("project"));
}

#[test]
fn invalid_path_ref_is_rejected() {
    let error = HostPath::from_path_ref("not-a-path-ref").expect_err("invalid ref must fail");
    assert_eq!(error, HostPathError::InvalidPathRef);
}

#[test]
fn canonicalize_returns_existing_native_path() {
    let directory = tempfile::tempdir().expect("temp directory should create");
    let canonical = HostPath::canonicalize(directory.path()).expect("path should canonicalize");

    assert!(canonical.as_path().is_absolute());
    assert!(canonical.as_path().is_dir());
}

#[cfg(windows)]
#[test]
fn canonicalize_uses_shell_compatible_windows_path_when_safe() {
    let directory = tempfile::tempdir().expect("temp directory should create");
    let canonical = HostPath::canonicalize(directory.path()).expect("path should canonicalize");
    let display = canonical.display_string();

    assert!(!display.starts_with(r"\\?\"));
    assert!(Path::new(&display).is_absolute());
}
