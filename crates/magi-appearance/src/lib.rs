mod assets;
mod builtin;
mod contract;
mod library;
mod package;
mod validator;

pub use assets::StoredAsset;
pub use contract::*;
pub use library::{
    AppearanceError, AppearanceErrorKind, AppearanceLibrary, ImportConflictStrategy,
};
pub use validator::{theme_content_hash, validate_theme, validate_user_theme};

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tempfile::tempdir;

    fn custom_theme(id: &str) -> ThemePack {
        ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: id.to_string(),
            name: "测试主题".to_string(),
            description: None,
            author: None,
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(ThemeScheme {
                    accent: "#58A477".to_string(),
                    background: "#101612".to_string(),
                    foreground: "#EEF4F0".to_string(),
                    contrast: 60,
                }),
            },
            material: ThemeMaterial::Translucent,
            wallpaper: None,
        }
    }

    #[test]
    fn library_persists_editable_user_themes_and_active_theme() {
        let root = tempdir().unwrap();
        let library = AppearanceLibrary::open(root.path()).unwrap();
        let snapshot = library.snapshot();
        assert_eq!(snapshot.active_theme_id, "builtin.system");
        let snapshot = library
            .create_theme(
                custom_theme("user.forest"),
                snapshot.revision,
                ThemeSource::Created,
            )
            .unwrap();
        assert!(
            snapshot
                .themes
                .iter()
                .any(|record| record.pack.id == "user.forest" && record.editable)
        );
        let snapshot = library.activate("user.forest", snapshot.revision).unwrap();
        assert_eq!(snapshot.active_theme_id, "user.forest");
        let reopened = AppearanceLibrary::open(root.path()).unwrap();
        assert_eq!(reopened.snapshot().active_theme_id, "user.forest");
    }

    #[test]
    fn builtin_theme_cannot_be_edited_or_exported() {
        let root = tempdir().unwrap();
        let library = AppearanceLibrary::open(root.path()).unwrap();
        let revision = library.snapshot().revision;
        assert!(
            library
                .update_theme("builtin.dark", custom_theme("builtin.dark"), revision)
                .is_err()
        );
        assert!(library.export_theme("builtin.dark").is_err());
    }

    #[test]
    fn user_theme_cannot_embed_system_color_mode() {
        let mut theme = custom_theme("user.adaptive");
        let dark = theme.schemes.dark.clone().unwrap();
        theme.scheme_policy = ThemeSchemePolicy::Adaptive;
        theme.schemes.light = Some(dark);

        let library = AppearanceLibrary::in_memory();
        let error = library
            .create_theme(theme, library.snapshot().revision, ThemeSource::Created)
            .expect_err("用户主题不应嵌套跟随系统模式");

        assert_eq!(error.to_string(), "用户主题必须固定为浅色或深色方案");
    }

    #[test]
    fn builtin_wallpaper_is_installed_and_survives_cleanup() {
        let library = AppearanceLibrary::in_memory();
        let snapshot = library.snapshot();
        let wallpaper = snapshot
            .themes
            .iter()
            .find(|record| record.pack.id == "builtin.forest")
            .and_then(|record| record.pack.wallpaper.as_ref())
            .expect("深林主题必须包含内置背景图");

        let (bytes, mime_type) = library
            .read_asset(&wallpaper.asset_id)
            .expect("内置背景图必须可读取");
        assert!(!bytes.is_empty());
        assert_eq!(mime_type, "image/webp");

        library
            .create_theme(
                custom_theme("user.cleanup-trigger"),
                snapshot.revision,
                ThemeSource::Created,
            )
            .expect("创建主题并触发资源清理必须成功");
        assert!(library.read_asset(&wallpaper.asset_id).is_ok());
    }

    #[test]
    fn theme_package_round_trip_keeps_import_editable() {
        let source_root = tempdir().unwrap();
        let source = AppearanceLibrary::open(source_root.path()).unwrap();
        let snapshot = source
            .create_theme(
                custom_theme("user.exported"),
                source.snapshot().revision,
                ThemeSource::Created,
            )
            .unwrap();
        let package = source.export_theme("user.exported").unwrap();
        let target_root = tempdir().unwrap();
        let target = AppearanceLibrary::open(target_root.path()).unwrap();
        let imported = target
            .import_theme(
                &package,
                target.snapshot().revision,
                ImportConflictStrategy::Reject,
            )
            .unwrap();
        let record = imported
            .themes
            .iter()
            .find(|record| record.pack.id == "user.exported")
            .unwrap();
        assert_eq!(record.source, ThemeSource::Imported);
        assert!(record.editable);
        assert_eq!(
            snapshot
                .themes
                .iter()
                .filter(|record| record.pack.id == "user.exported")
                .count(),
            1
        );
    }

    #[test]
    fn duplicate_import_creates_an_independent_theme_even_when_content_matches() {
        let root = tempdir().unwrap();
        let library = AppearanceLibrary::open(root.path()).unwrap();
        let snapshot = library
            .create_theme(
                custom_theme("user.exported"),
                library.snapshot().revision,
                ThemeSource::Created,
            )
            .unwrap();
        let package = library.export_theme("user.exported").unwrap();

        let imported = library
            .import_theme(
                &package,
                snapshot.revision,
                ImportConflictStrategy::Duplicate,
            )
            .unwrap();

        let duplicate = imported
            .themes
            .iter()
            .find(|record| record.pack.id.starts_with("user.imported-"))
            .expect("复制导入必须创建独立主题");
        assert_eq!(duplicate.source, ThemeSource::Imported);
        assert_eq!(duplicate.pack.name, "测试主题");
        assert_ne!(duplicate.pack.id, "user.exported");
    }

    #[test]
    fn staged_wallpaper_survives_unrelated_changes_and_is_cleaned_after_delete() {
        let root = tempdir().unwrap();
        let library = AppearanceLibrary::open(root.path()).unwrap();
        let png = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
        let asset = library.put_asset(&png).unwrap();

        let snapshot = library
            .create_theme(
                custom_theme("user.unrelated"),
                library.snapshot().revision,
                ThemeSource::Created,
            )
            .unwrap();
        assert!(library.read_asset(&asset.asset_id).is_ok());

        let mut wallpaper_theme = custom_theme("user.wallpaper");
        wallpaper_theme.wallpaper = Some(WallpaperDefinition {
            asset_id: asset.asset_id.clone(),
            focus_x: 0.5,
            focus_y: 0.5,
            dim: 0.2,
            blur: 0,
        });
        let snapshot = library
            .create_theme(wallpaper_theme, snapshot.revision, ThemeSource::Created)
            .unwrap();
        let snapshot = library
            .delete_theme("user.wallpaper", snapshot.revision)
            .unwrap();
        assert!(
            snapshot
                .themes
                .iter()
                .all(|record| record.pack.id != "user.wallpaper")
        );
        assert!(library.read_asset(&asset.asset_id).is_err());
    }
}
