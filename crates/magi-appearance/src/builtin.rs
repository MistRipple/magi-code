use crate::contract::*;
use crate::validator::theme_content_hash;

const CREATED_AT: u64 = 1;

pub(crate) struct BuiltinAsset {
    pub asset_id: &'static str,
    pub bytes: &'static [u8],
}

const DEEP_FOREST_WALLPAPER: BuiltinAsset = BuiltinAsset {
    asset_id: "d57b3e662d0536b02d6206fd836b0aa12b47391d1cb924b8886cc25e336c0418",
    bytes: include_bytes!("../assets/deep-forest.webp"),
};
const STARRY_SNOW_MOUNTAIN_WALLPAPER: BuiltinAsset = BuiltinAsset {
    asset_id: "801679e19cec00972022d6be4bd60330ac0e169ea92d05cf277e32ea9d5c5fbf",
    bytes: include_bytes!("../assets/starry-snow-mountain.webp"),
};
const ANIME_SHRINE_WALLPAPER: BuiltinAsset = BuiltinAsset {
    asset_id: "485cacfbc89a5df085bacd9018f5d81adb10147f83eacf45bc1ad1a60a07c65f",
    bytes: include_bytes!("../assets/anime-shrine.webp"),
};
const COASTAL_DAWN_WALLPAPER: BuiltinAsset = BuiltinAsset {
    asset_id: "137255641123991b8f49f09de716936cc6cdf28e28df84bb513ef7c217f2fbcf",
    bytes: include_bytes!("../assets/coastal-dawn.webp"),
};
const DESERT_DAWN_WALLPAPER: BuiltinAsset = BuiltinAsset {
    asset_id: "aeace23a9c28bfbcf932e614e43dc0168b4f22ed96d68238e1b3c7a6be02399d",
    bytes: include_bytes!("../assets/desert-dawn.webp"),
};
const MAGI_ORBITAL: BuiltinAsset = BuiltinAsset {
    asset_id: "86e66f57acecb7e19b4695baaab442bd1a09d6580a4904b2805ea35758134676",
    bytes: include_bytes!("../assets/magi-orbital.webp"),
};
const MAGI_CYBER_RAIN: BuiltinAsset = BuiltinAsset {
    asset_id: "d9b3de27c43934f73111b65bc6868c21c6304919441be4cba9bef449e6001b7f",
    bytes: include_bytes!("../assets/magi-cyber-rain.webp"),
};
const MAGI_CELESTIAL: BuiltinAsset = BuiltinAsset {
    asset_id: "d59378970629c50211d82c071a15ba33fde1e25a0240cbcc59d2a1109e212b55",
    bytes: include_bytes!("../assets/magi-celestial.webp"),
};
const MAGI_AURORA: BuiltinAsset = BuiltinAsset {
    asset_id: "77570c342e51919468f8329cef87e4f86f025be80b5d21e7916d444a6a4f1a1b",
    bytes: include_bytes!("../assets/magi-aurora.webp"),
};

pub(crate) fn builtin_assets() -> [BuiltinAsset; 9] {
    [
        MAGI_ORBITAL,
        MAGI_CYBER_RAIN,
        MAGI_CELESTIAL,
        MAGI_AURORA,
        DEEP_FOREST_WALLPAPER,
        STARRY_SNOW_MOUNTAIN_WALLPAPER,
        ANIME_SHRINE_WALLPAPER,
        COASTAL_DAWN_WALLPAPER,
        DESERT_DAWN_WALLPAPER,
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

fn wallpaper(
    asset: BuiltinAsset,
    focus_x: f32,
    focus_y: f32,
    dim: f32,
    blur: u8,
) -> WallpaperDefinition {
    WallpaperDefinition {
        asset_id: asset.asset_id.to_string(),
        focus_x,
        focus_y,
        dim,
        blur,
    }
}

/// 内置主题的可变部分；作者、版本与记录元数据对所有内置主题一致。
struct BuiltinTheme {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    scheme_policy: ThemeSchemePolicy,
    schemes: ThemeSchemes,
    material: ThemeMaterial,
    wallpaper: Option<WallpaperDefinition>,
}

impl BuiltinTheme {
    fn dark(
        id: &'static str,
        name: &'static str,
        description: &'static str,
        dark: ThemeScheme,
        material: ThemeMaterial,
        wallpaper: Option<WallpaperDefinition>,
    ) -> Self {
        Self {
            id,
            name,
            description,
            scheme_policy: ThemeSchemePolicy::Dark,
            schemes: ThemeSchemes {
                light: None,
                dark: Some(dark),
            },
            material,
            wallpaper,
        }
    }

    fn light(
        id: &'static str,
        name: &'static str,
        description: &'static str,
        light: ThemeScheme,
        material: ThemeMaterial,
        wallpaper: Option<WallpaperDefinition>,
    ) -> Self {
        Self {
            id,
            name,
            description,
            scheme_policy: ThemeSchemePolicy::Light,
            schemes: ThemeSchemes {
                light: Some(light),
                dark: None,
            },
            material,
            wallpaper,
        }
    }

    fn into_record(self) -> ThemeRecord {
        let pack = ThemePack {
            schema_version: THEME_SCHEMA_VERSION,
            id: self.id.to_string(),
            name: self.name.to_string(),
            description: Some(self.description.to_string()),
            author: Some("Magi".to_string()),
            scheme_policy: self.scheme_policy,
            schemes: self.schemes,
            material: self.material,
            wallpaper: self.wallpaper,
        };
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
}

pub fn builtin_themes() -> Vec<ThemeRecord> {
    use ThemeMaterial::{Clear, Immersive, Translucent};
    let light = scheme("#2563EB", "#FCFCFB", "#272725", 52);
    let dark = scheme("#3B82F6", "#0B0B0A", "#E7E7E4", 62);
    let system = BuiltinTheme {
        id: "builtin.system",
        name: "跟随系统",
        description: "自动使用 Magi 浅色或深色外观",
        scheme_policy: ThemeSchemePolicy::Adaptive,
        schemes: ThemeSchemes {
            light: Some(light.clone()),
            dark: Some(dark.clone()),
        },
        material: Clear,
        wallpaper: None,
    };
    [
        system,
        BuiltinTheme::light(
            "builtin.light",
            "Magi 浅色",
            "清晰克制的浅色工作台",
            light,
            Clear,
            None,
        ),
        BuiltinTheme::dark(
            "builtin.dark",
            "Magi 深色",
            "适合长时间工作的深色工作台",
            dark,
            Clear,
            None,
        ),
        BuiltinTheme::dark(
            "builtin.forest",
            "深林",
            "低饱和绿色强调的沉静主题",
            scheme("#5FA77A", "#101612", "#E9F1EC", 58),
            Translucent,
            Some(wallpaper(DEEP_FOREST_WALLPAPER, 0.62, 0.5, 0.4, 2)),
        ),
        BuiltinTheme::dark(
            "builtin.starry-snow-mountain",
            "星夜雪山",
            "蓝紫星河与雪山构成的沉浸深色主题",
            scheme("#B7A6FF", "#0B1324", "#EDF2FF", 64),
            Immersive,
            Some(wallpaper(
                STARRY_SNOW_MOUNTAIN_WALLPAPER,
                0.52,
                0.42,
                0.32,
                0,
            )),
        ),
        BuiltinTheme::light(
            "builtin.anime-shrine",
            "青岚绘境",
            "青山神社意象的日系动画风浅色主题",
            scheme("#A33B36", "#F2F0DD", "#252A20", 55),
            Translucent,
            Some(wallpaper(ANIME_SHRINE_WALLPAPER, 0.64, 0.46, 0.16, 1)),
        ),
        BuiltinTheme::light(
            "builtin.coastal-dawn",
            "海岸晨光",
            "浅青海面与暖色晨曦组成的通透浅色主题",
            scheme("#087F8C", "#EEF8F7", "#173538", 53),
            Translucent,
            Some(wallpaper(COASTAL_DAWN_WALLPAPER, 0.58, 0.5, 0.12, 1)),
        ),
        BuiltinTheme::dark(
            "builtin.desert-dawn",
            "赤砂远境",
            "赤砂与暮蓝天空构成的暖调沉浸主题",
            scheme("#F28C4B", "#1D110D", "#F7E9DE", 64),
            Immersive,
            Some(wallpaper(DESERT_DAWN_WALLPAPER, 0.54, 0.5, 0.38, 0)),
        ),
        BuiltinTheme::dark(
            "builtin.magi-orbital",
            "轨道圣域",
            "轨道光环与机械仆从环绕的紫色圣域",
            scheme("#C4A1F3", "#170E23", "#F3EDF8", 64),
            Immersive,
            Some(wallpaper(MAGI_ORBITAL, 0.78, 0.45, 0.3, 0)),
        ),
        BuiltinTheme::dark(
            "builtin.magi-cyber-rain",
            "霓虹雨城",
            "霓虹雨幕中的跃迁与仆从出击",
            scheme("#68D5DD", "#10262D", "#E7F3F3", 64),
            Immersive,
            Some(wallpaper(MAGI_CYBER_RAIN, 0.78, 0.45, 0.38, 0)),
        ),
        BuiltinTheme::dark(
            "builtin.magi-celestial",
            "星穹书库",
            "星图与悬浮书库交织的知识空间",
            scheme("#B3BCEB", "#192238", "#ECEFF7", 60),
            Immersive,
            Some(wallpaper(MAGI_CELESTIAL, 0.78, 0.45, 0.38, 0)),
        ),
        BuiltinTheme::dark(
            "builtin.magi-aurora",
            "极光指挥台",
            "极光天幕下的云端指挥与仆从派遣",
            scheme("#EDB58E", "#29222B", "#F6EDE6", 62),
            Immersive,
            Some(wallpaper(MAGI_AURORA, 0.78, 0.45, 0.42, 0)),
        ),
    ]
    .into_iter()
    .map(BuiltinTheme::into_record)
    .collect()
}
