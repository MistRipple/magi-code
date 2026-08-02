use crate::contract::*;
use crate::validator::theme_content_hash;

const CREATED_AT: u64 = 1;
const DEEP_FOREST_WALLPAPER_ASSET_ID: &str =
    "d57b3e662d0536b02d6206fd836b0aa12b47391d1cb924b8886cc25e336c0418";
const DEEP_FOREST_WALLPAPER_BYTES: &[u8] = include_bytes!("../assets/deep-forest.webp");

pub(crate) struct BuiltinAsset {
    pub asset_id: &'static str,
    pub bytes: &'static [u8],
}

pub(crate) fn builtin_assets() -> [BuiltinAsset; 1] {
    [BuiltinAsset {
        asset_id: DEEP_FOREST_WALLPAPER_ASSET_ID,
        bytes: DEEP_FOREST_WALLPAPER_BYTES,
    }]
}

fn scheme(accent: &str, background: &str, foreground: &str, contrast: u8) -> ThemeScheme {
    ThemeScheme {
        accent: accent.to_string(),
        background: background.to_string(),
        foreground: foreground.to_string(),
        contrast,
    }
}

fn record(pack: ThemePack) -> ThemeRecord {
    ThemeRecord {
        content_hash: theme_content_hash(&pack).expect("内置主题必须可以序列化"),
        pack,
        source: ThemeSource::Builtin,
        editable: false,
        revision: 1,
        created_at: CREATED_AT,
        updated_at: CREATED_AT,
    }
}

pub fn builtin_themes() -> Vec<ThemeRecord> {
    let light = scheme("#2563EB", "#FFFFFF", "#1F2937", 52);
    let dark = scheme("#3B82F6", "#0F141B", "#E5E7EB", 62);
    vec![
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.system".to_string(),
            name: "跟随系统".to_string(),
            description: Some("自动使用 Magi 浅色或深色外观".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Adaptive,
            schemes: ThemeSchemes {
                light: Some(light.clone()),
                dark: Some(dark.clone()),
            },
            material: ThemeMaterial::Clear,
            wallpaper: None,
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.light".to_string(),
            name: "Magi 浅色".to_string(),
            description: Some("清晰克制的浅色工作台".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Light,
            schemes: ThemeSchemes {
                light: Some(light),
                dark: None,
            },
            material: ThemeMaterial::Clear,
            wallpaper: None,
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.dark".to_string(),
            name: "Magi 深色".to_string(),
            description: Some("适合长时间工作的深色工作台".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(dark),
            },
            material: ThemeMaterial::Clear,
            wallpaper: None,
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.forest".to_string(),
            name: "深林".to_string(),
            description: Some("低饱和绿色强调的沉静主题".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(scheme("#5FA77A", "#101612", "#E9F1EC", 58)),
            },
            material: ThemeMaterial::Translucent,
            wallpaper: Some(WallpaperDefinition {
                asset_id: DEEP_FOREST_WALLPAPER_ASSET_ID.to_string(),
                focus_x: 0.62,
                focus_y: 0.5,
                dim: 0.4,
                blur: 2,
            }),
        }),
    ]
}
