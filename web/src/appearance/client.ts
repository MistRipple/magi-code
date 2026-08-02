import { getTransport } from '../shared/transport';
import { agentUrl } from '../web/agent-api';
import {
  normalizeAppearanceSnapshot,
  type AppearanceAssetPayload,
  type AppearanceAssetUploadResult,
  type AppearanceSnapshot,
  type ThemePack,
} from './contract';

async function parseJson<T>(response: Response, action: string): Promise<T> {
  if (!response.ok) {
    let message = `${action}失败（HTTP ${response.status}）`;
    try {
      const payload = await response.json() as { message?: unknown; detail?: unknown; error?: unknown };
      const detail = typeof payload.detail === 'string' && payload.detail.trim()
        ? payload.detail
        : typeof payload.message === 'string'
          ? payload.message
          : payload.error;
      if (typeof detail === 'string' && detail.trim()) message = detail.trim();
    } catch {
      // HTTP 状态已经保留在默认错误中。
    }
    throw new Error(message);
  }
  return await response.json() as T;
}

async function getJson<T>(path: string, action: string): Promise<T> {
  return await parseJson<T>(await getTransport().request(agentUrl(path)), action);
}

async function postJson<T>(path: string, body: unknown, action: string): Promise<T> {
  const response = await getTransport().request(agentUrl(path), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  return await parseJson<T>(response, action);
}

export async function fetchAppearanceSnapshot(): Promise<AppearanceSnapshot> {
  return normalizeAppearanceSnapshot(await getJson('/api/appearance/bootstrap', '加载外观'));
}

export async function activateAppearanceTheme(themeId: string, expectedRevision: number): Promise<AppearanceSnapshot> {
  return normalizeAppearanceSnapshot(await postJson('/api/appearance/activate', { themeId, expectedRevision }, '应用主题'));
}

export async function createAppearanceTheme(pack: ThemePack, expectedRevision: number): Promise<AppearanceSnapshot> {
  return normalizeAppearanceSnapshot(await postJson('/api/appearance/themes', { pack, expectedRevision }, '创建主题'));
}

export async function updateAppearanceTheme(themeId: string, pack: ThemePack, expectedRevision: number): Promise<AppearanceSnapshot> {
  return normalizeAppearanceSnapshot(await postJson('/api/appearance/themes/update', { themeId, pack, expectedRevision }, '保存主题'));
}

export async function deleteAppearanceTheme(themeId: string, expectedRevision: number): Promise<AppearanceSnapshot> {
  return normalizeAppearanceSnapshot(await postJson(
    `/api/appearance/themes/${encodeURIComponent(themeId)}/delete`,
    { expectedRevision },
    '删除主题',
  ));
}

export async function uploadAppearanceAsset(file: File): Promise<AppearanceAssetUploadResult> {
  return await postJson('/api/appearance/assets', { dataBase64: await fileToBase64(file) }, '上传背景图');
}

export async function fetchAppearanceAsset(assetId: string): Promise<AppearanceAssetPayload> {
  return await getJson(`/api/appearance/assets/${encodeURIComponent(assetId)}`, '加载背景图');
}

export async function importAppearanceTheme(
  file: File,
  expectedRevision: number,
  conflictStrategy: 'reject' | 'duplicate' | 'replace' = 'reject',
): Promise<AppearanceSnapshot> {
  return normalizeAppearanceSnapshot(await postJson('/api/appearance/themes/import', {
    expectedRevision,
    packageBase64: await fileToBase64(file),
    conflictStrategy,
  }, '导入主题'));
}

export async function exportAppearanceTheme(themeId: string): Promise<{ fileName: string; packageBase64: string }> {
  return await getJson(`/api/appearance/themes/${encodeURIComponent(themeId)}/export`, '导出主题');
}

export function downloadThemePackage(payload: { fileName: string; packageBase64: string }): void {
  const bytes = base64ToBytes(payload.packageBase64);
  const arrayBuffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
  const url = URL.createObjectURL(new Blob([arrayBuffer], { type: 'application/zip' }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = payload.fileName;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

async function fileToBase64(file: File): Promise<string> {
  return bytesToBase64(new Uint8Array(await file.arrayBuffer()));
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

export function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}
