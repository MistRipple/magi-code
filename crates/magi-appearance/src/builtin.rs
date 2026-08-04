use crate::contract::*;
use crate::validator::theme_content_hash;

const CREATED_AT: u64 = 1;
const DEEP_FOREST_WALLPAPER_ASSET_ID: &str =
    "d57b3e662d0536b02d6206fd836b0aa12b47391d1cb924b8886cc25e336c0418";
const DEEP_FOREST_WALLPAPER_BYTES: &[u8] = include_bytes!("../assets/deep-forest.webp");
const STARRY_SNOW_MOUNTAIN_WALLPAPER_ASSET_ID: &str =
    "801679e19cec00972022d6be4bd60330ac0e169ea92d05cf277e32ea9d5c5fbf";
const STARRY_SNOW_MOUNTAIN_WALLPAPER_BYTES: &[u8] =
    include_bytes!("../assets/starry-snow-mountain.webp");
const ANIME_SHRINE_WALLPAPER_ASSET_ID: &str =
    "485cacfbc89a5df085bacd9018f5d81adb10147f83eacf45bc1ad1a60a07c65f";
const ANIME_SHRINE_WALLPAPER_BYTES: &[u8] = include_bytes!("../assets/anime-shrine.webp");
const QUANTUM_GRID_WALLPAPER_ASSET_ID: &str =
    "bac7ef1d63f643c7f21bfa729bdae768058e15143ba97ea8eaab94a6059e238c";
const QUANTUM_GRID_WALLPAPER_BYTES: &[u8] = include_bytes!("../assets/quantum-grid.webp");
const COASTAL_DAWN_WALLPAPER_ASSET_ID: &str =
    "137255641123991b8f49f09de716936cc6cdf28e28df84bb513ef7c217f2fbcf";
const COASTAL_DAWN_WALLPAPER_BYTES: &[u8] = include_bytes!("../assets/coastal-dawn.webp");
const DESERT_DAWN_WALLPAPER_ASSET_ID: &str =
    "aeace23a9c28bfbcf932e614e43dc0168b4f22ed96d68238e1b3c7a6be02399d";
const DESERT_DAWN_WALLPAPER_BYTES: &[u8] = include_bytes!("../assets/desert-dawn.webp");

pub(crate) struct BuiltinAsset {
    pub asset_id: &'static str,
    pub bytes: &'static [u8],
}

pub(crate) fn builtin_assets() -> [BuiltinAsset; 6] {
    [
        BuiltinAsset {
            asset_id: DEEP_FOREST_WALLPAPER_ASSET_ID,
            bytes: DEEP_FOREST_WALLPAPER_BYTES,
        },
        BuiltinAsset {
            asset_id: STARRY_SNOW_MOUNTAIN_WALLPAPER_ASSET_ID,
            bytes: STARRY_SNOW_MOUNTAIN_WALLPAPER_BYTES,
        },
        BuiltinAsset {
            asset_id: ANIME_SHRINE_WALLPAPER_ASSET_ID,
            bytes: ANIME_SHRINE_WALLPAPER_BYTES,
        },
        BuiltinAsset {
            asset_id: QUANTUM_GRID_WALLPAPER_ASSET_ID,
            bytes: QUANTUM_GRID_WALLPAPER_BYTES,
        },
        BuiltinAsset {
            asset_id: COASTAL_DAWN_WALLPAPER_ASSET_ID,
            bytes: COASTAL_DAWN_WALLPAPER_BYTES,
        },
        BuiltinAsset {
            asset_id: DESERT_DAWN_WALLPAPER_ASSET_ID,
            bytes: DESERT_DAWN_WALLPAPER_BYTES,
        },
    ]
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

fn wallpaper(
    asset_id: &str,
    focus_x: f32,
    focus_y: f32,
    dim: f32,
    blur: u8,
) -> WallpaperDefinition {
    WallpaperDefinition {
        asset_id: asset_id.to_string(),
        focus_x,
        focus_y,
        dim,
        blur,
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
            wallpaper: Some(wallpaper(DEEP_FOREST_WALLPAPER_ASSET_ID, 0.62, 0.5, 0.4, 2)),
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.starry-snow-mountain".to_string(),
            name: "星夜雪山".to_string(),
            description: Some("蓝紫星河与雪山构成的沉浸深色主题".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(scheme("#B7A6FF", "#0B1324", "#EDF2FF", 64)),
            },
            material: ThemeMaterial::Immersive,
            wallpaper: Some(wallpaper(
                STARRY_SNOW_MOUNTAIN_WALLPAPER_ASSET_ID,
                0.52,
                0.42,
                0.32,
                0,
            )),
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.anime-shrine".to_string(),
            name: "青岚绘境".to_string(),
            description: Some("青山神社意象的日系动画风浅色主题".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Light,
            schemes: ThemeSchemes {
                light: Some(scheme("#A33B36", "#F2F0DD", "#252A20", 55)),
                dark: None,
            },
            material: ThemeMaterial::Translucent,
            wallpaper: Some(wallpaper(
                ANIME_SHRINE_WALLPAPER_ASSET_ID,
                0.64,
                0.46,
                0.16,
                1,
            )),
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.quantum-grid".to_string(),
            name: "量子矩阵".to_string(),
            description: Some("冷蓝数据空间构成的高对比科技主题".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(scheme("#42D9FF", "#06111F", "#DFF7FF", 68)),
            },
            material: ThemeMaterial::Immersive,
            wallpaper: Some(wallpaper(
                QUANTUM_GRID_WALLPAPER_ASSET_ID,
                0.58,
                0.48,
                0.44,
                1,
            )),
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.coastal-dawn".to_string(),
            name: "海岸晨光".to_string(),
            description: Some("浅青海面与暖色晨曦组成的通透浅色主题".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Light,
            schemes: ThemeSchemes {
                light: Some(scheme("#087F8C", "#EEF8F7", "#173538", 53)),
                dark: None,
            },
            material: ThemeMaterial::Translucent,
            wallpaper: Some(wallpaper(
                COASTAL_DAWN_WALLPAPER_ASSET_ID,
                0.58,
                0.5,
                0.12,
                1,
            )),
        }),
        record(ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: "builtin.desert-dawn".to_string(),
            name: "赤砂远境".to_string(),
            description: Some("赤砂与暮蓝天空构成的暖调沉浸主题".to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(scheme("#F28C4B", "#1D110D", "#F7E9DE", 64)),
            },
            material: ThemeMaterial::Immersive,
            wallpaper: Some(wallpaper(
                DESERT_DAWN_WALLPAPER_ASSET_ID,
                0.54,
                0.5,
                0.38,
                0,
            )),
        }),
    ]
}
