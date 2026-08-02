export type ThemeSchemePolicy = 'light' | 'dark' | 'adaptive';
export type ThemeMaterial = 'clear' | 'translucent' | 'immersive';
export type ThemeSource = 'builtin' | 'created' | 'imported';

export interface ThemeScheme {
  accent: string;
  background: string;
  foreground: string;
  contrast: number;
}

export interface ThemePack {
  schemaVersion: 1;
  id: string;
  name: string;
  description?: string;
  author?: string;
  schemePolicy: ThemeSchemePolicy;
  schemes: {
    light?: ThemeScheme;
    dark?: ThemeScheme;
  };
  material: ThemeMaterial;
  wallpaper?: {
    assetId: string;
    focusX: number;
    focusY: number;
    dim: number;
    blur: number;
  };
}

export interface ThemeRecord {
  pack: ThemePack;
  source: ThemeSource;
  editable: boolean;
  revision: number;
  contentHash: string;
  createdAt: number;
  updatedAt: number;
}

export interface AppearanceSnapshot {
  revision: number;
  activeThemeId: string;
  themes: ThemeRecord[];
}

export interface AppearanceAssetPayload {
  assetId: string;
  mimeType: string;
  dataBase64: string;
}

export interface AppearanceAssetUploadResult {
  assetId: string;
  mimeType: string;
  width: number;
  height: number;
}

export function normalizeAppearanceSnapshot(value: unknown): AppearanceSnapshot {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('外观状态格式无效');
  }
  const payload = value as Record<string, unknown>;
  const revision = Number(payload.revision);
  const activeThemeId = typeof payload.activeThemeId === 'string' ? payload.activeThemeId.trim() : '';
  if (!Number.isSafeInteger(revision) || revision < 1 || !activeThemeId || !Array.isArray(payload.themes)) {
    throw new Error('外观状态缺少必要字段');
  }
  const themes = payload.themes.map(normalizeThemeRecord);
  if (!themes.some((record) => record.pack.id === activeThemeId)) {
    throw new Error('当前外观主题不存在');
  }
  return { revision, activeThemeId, themes };
}

function normalizeThemeRecord(value: unknown): ThemeRecord {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('主题记录格式无效');
  }
  const record = value as Record<string, unknown>;
  const source = record.source;
  if (source !== 'builtin' && source !== 'created' && source !== 'imported') {
    throw new Error('主题来源无效');
  }
  return {
    pack: normalizeThemePack(record.pack),
    source,
    editable: record.editable === true,
    revision: Number(record.revision) || 1,
    contentHash: typeof record.contentHash === 'string' ? record.contentHash : '',
    createdAt: Number(record.createdAt) || 0,
    updatedAt: Number(record.updatedAt) || 0,
  };
}

export function normalizeThemePack(value: unknown): ThemePack {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('主题定义格式无效');
  }
  const pack = value as Record<string, unknown>;
  const schemes = pack.schemes && typeof pack.schemes === 'object' && !Array.isArray(pack.schemes)
    ? pack.schemes as Record<string, unknown>
    : {};
  const policy = pack.schemePolicy;
  const material = pack.material;
  if (pack.schemaVersion !== 1 || (policy !== 'light' && policy !== 'dark' && policy !== 'adaptive')) {
    throw new Error('主题协议不受支持');
  }
  if (material !== 'clear' && material !== 'translucent' && material !== 'immersive') {
    throw new Error('主题材质无效');
  }
  const result: ThemePack = {
    schemaVersion: 1,
    id: typeof pack.id === 'string' ? pack.id.trim() : '',
    name: typeof pack.name === 'string' ? pack.name.trim() : '',
    ...(typeof pack.description === 'string' ? { description: pack.description } : {}),
    ...(typeof pack.author === 'string' ? { author: pack.author } : {}),
    schemePolicy: policy,
    schemes: {
      ...(schemes.light ? { light: normalizeThemeScheme(schemes.light) } : {}),
      ...(schemes.dark ? { dark: normalizeThemeScheme(schemes.dark) } : {}),
    },
    material,
  };
  if (pack.wallpaper && typeof pack.wallpaper === 'object' && !Array.isArray(pack.wallpaper)) {
    const wallpaper = pack.wallpaper as Record<string, unknown>;
    result.wallpaper = {
      assetId: typeof wallpaper.assetId === 'string' ? wallpaper.assetId : '',
      focusX: Number(wallpaper.focusX),
      focusY: Number(wallpaper.focusY),
      dim: Number(wallpaper.dim),
      blur: Number(wallpaper.blur),
    };
  }
  if (!result.id || !result.name) throw new Error('主题标识或名称无效');
  return result;
}

function normalizeThemeScheme(value: unknown): ThemeScheme {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('主题配色格式无效');
  }
  const scheme = value as Record<string, unknown>;
  return {
    accent: String(scheme.accent || ''),
    background: String(scheme.background || ''),
    foreground: String(scheme.foreground || ''),
    contrast: Number(scheme.contrast),
  };
}
