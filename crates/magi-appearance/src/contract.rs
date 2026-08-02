use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const THEME_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemePack {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub scheme_policy: ThemeSchemePolicy,
    pub schemes: ThemeSchemes,
    pub material: ThemeMaterial,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper: Option<WallpaperDefinition>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThemeSchemePolicy {
    Light,
    Dark,
    Adaptive,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSchemes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<ThemeScheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<ThemeScheme>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemeScheme {
    pub accent: String,
    pub background: String,
    pub foreground: String,
    pub contrast: u8,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMaterial {
    Clear,
    Translucent,
    Immersive,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperDefinition {
    pub asset_id: String,
    pub focus_x: f32,
    pub focus_y: f32,
    pub dim: f32,
    pub blur: u8,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ThemeSource {
    Builtin,
    Created,
    Imported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemeRecord {
    pub pack: ThemePack,
    pub source: ThemeSource,
    pub editable: bool,
    pub revision: u64,
    pub content_hash: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSnapshot {
    pub revision: u64,
    pub active_theme_id: String,
    pub themes: Vec<ThemeRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedAppearanceState {
    pub revision: u64,
    pub active_theme_id: String,
    pub user_themes: Vec<ThemeRecord>,
    #[serde(default)]
    pub staged_assets: BTreeMap<String, u64>,
}
