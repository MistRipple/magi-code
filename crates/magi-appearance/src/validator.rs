use crate::contract::{
    THEME_SCHEMA_VERSION, ThemePack, ThemeScheme, ThemeSchemePolicy, ThemeSource,
};
use sha2::{Digest, Sha256};

pub fn validate_theme(pack: &ThemePack) -> Result<(), String> {
    if pack.schema_version != THEME_SCHEMA_VERSION {
        return Err(format!("不支持的主题协议版本: {}", pack.schema_version));
    }
    validate_identifier(&pack.id)?;
    validate_text(&pack.name, "主题名称", 1, 64)?;
    if let Some(value) = pack.description.as_deref() {
        validate_text(value, "主题说明", 0, 240)?;
    }
    if let Some(value) = pack.author.as_deref() {
        validate_text(value, "主题作者", 0, 80)?;
    }
    match pack.scheme_policy {
        ThemeSchemePolicy::Light => {
            validate_scheme(pack.schemes.light.as_ref(), "浅色")?;
            if pack.schemes.dark.is_some() {
                return Err("固定浅色主题不能包含深色方案".to_string());
            }
        }
        ThemeSchemePolicy::Dark => {
            validate_scheme(pack.schemes.dark.as_ref(), "深色")?;
            if pack.schemes.light.is_some() {
                return Err("固定深色主题不能包含浅色方案".to_string());
            }
        }
        ThemeSchemePolicy::Adaptive => {
            validate_scheme(pack.schemes.light.as_ref(), "浅色")?;
            validate_scheme(pack.schemes.dark.as_ref(), "深色")?;
        }
    }
    if let Some(wallpaper) = &pack.wallpaper {
        if wallpaper.asset_id.len() != 64
            || !wallpaper
                .asset_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("背景图资源标识无效".to_string());
        }
        if !(0.0..=1.0).contains(&wallpaper.focus_x)
            || !(0.0..=1.0).contains(&wallpaper.focus_y)
            || !(0.0..=0.85).contains(&wallpaper.dim)
            || wallpaper.blur > 24
        {
            return Err("背景图显示参数超出允许范围".to_string());
        }
    }
    Ok(())
}

pub fn validate_user_theme(pack: &ThemePack) -> Result<(), String> {
    validate_theme(pack)?;
    if pack.id.starts_with("builtin.") {
        return Err("用户主题不能使用内置主题标识".to_string());
    }
    if pack.scheme_policy == ThemeSchemePolicy::Adaptive {
        return Err("用户主题必须固定为浅色或深色方案".to_string());
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), String> {
    let valid = (3..=80).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase());
    if valid {
        Ok(())
    } else {
        Err("主题标识只能包含小写字母、数字、点、横线和下划线".to_string())
    }
}

fn validate_text(value: &str, label: &str, min: usize, max: usize) -> Result<(), String> {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    if count < min || count > max || trimmed.chars().any(char::is_control) {
        Err(format!("{label}无效"))
    } else {
        Ok(())
    }
}

fn validate_scheme(scheme: Option<&ThemeScheme>, label: &str) -> Result<(), String> {
    let scheme = scheme.ok_or_else(|| format!("缺少{label}主题方案"))?;
    let background =
        parse_color(&scheme.background).ok_or_else(|| format!("{label}背景颜色格式无效"))?;
    let foreground =
        parse_color(&scheme.foreground).ok_or_else(|| format!("{label}文字颜色格式无效"))?;
    if parse_color(&scheme.accent).is_none() {
        return Err(format!("{label}强调色格式无效"));
    }
    if contrast_ratio(background, foreground) < 4.5 {
        return Err(format!("{label}文字与背景对比度低于 WCAG AA"));
    }
    if !(20..=90).contains(&scheme.contrast) {
        return Err(format!("{label}界面对比度必须在 20 到 90 之间"));
    }
    Ok(())
}

fn parse_color(value: &str) -> Option<[u8; 3]> {
    let bytes = value.strip_prefix('#')?.as_bytes();
    if bytes.len() != 6 {
        return None;
    }
    let parse =
        |offset| u8::from_str_radix(std::str::from_utf8(&bytes[offset..offset + 2]).ok()?, 16).ok();
    Some([parse(0)?, parse(2)?, parse(4)?])
}

fn contrast_ratio(left: [u8; 3], right: [u8; 3]) -> f64 {
    let luminance = |color: [u8; 3]| {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color[0]) + 0.7152 * channel(color[1]) + 0.0722 * channel(color[2])
    };
    let left = luminance(left);
    let right = luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

pub fn theme_content_hash(pack: &ThemePack) -> Result<String, String> {
    let payload = serde_json::to_vec(pack).map_err(|error| format!("序列化主题失败: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(payload)))
}

pub fn editable_for_source(source: ThemeSource) -> bool {
    source != ThemeSource::Builtin
}
