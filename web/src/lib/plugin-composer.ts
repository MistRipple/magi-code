import { MAX_COMPOSER_IMAGE_BYTES, MAX_COMPOSER_TEXT_CHARS, isSupportedComposerDataUrl } from './composer-policy';
import type { PluginComposerImageInput, PluginComposerSnapshot, PluginComposerTarget, PluginComposerTextInput, PluginComposerTextReceipt } from '../shared/app-server-protocol.generated';
import { i18n } from '../stores/i18n.svelte';

export type PluginComposerImage = PluginComposerImageInput;

export interface PluginComposerPort {
  sessionId(): string;
  add(images: PluginComposerImage[]): string[] | null;
  has(id: string): boolean;
  writable(): boolean;
  textWritable(): boolean;
  beginRead(count: number): (() => void) | null;
  remove(id: string): boolean;
  focus(): void;
  notify(level: 'info' | 'warning' | 'error' | 'success', message: string): void;
  submitText(input: PluginComposerTextInput, admit: () => Promise<void>): Promise<PluginComposerTextReceipt>;
}

// 仅持有当前 InputArea 的调用端口，不保存第二份草稿或会话列表。
let active: PluginComposerPort | undefined;
interface TextSubmission { port: PluginComposerPort; input: PluginComposerTextInput; result: Promise<PluginComposerTextReceipt> }
let submittingText: TextSubmission | undefined;
const listeners = new Set<() => void>();
export function notifyPluginComposerChanged(): void { for (const listener of listeners) listener(); }
export function subscribePluginComposer(listener: () => void): () => void { listeners.add(listener); return () => listeners.delete(listener); }
export function registerPluginComposer(port: PluginComposerPort): () => void {
  if (active) throw new Error('当前窗口已有输入框端口');
  active = port;
  notifyPluginComposerChanged();
  return () => { if (active === port) { active = undefined; notifyPluginComposerChanged(); } };
}

export function pluginComposerSession(): string | null { return active?.sessionId() || null; }
export function pluginComposerSnapshot(ids: Iterable<string>, target: PluginComposerTarget = 'images'): PluginComposerSnapshot {
  return { sessionId: pluginComposerSession(), attachmentIds: active ? [...ids].filter((id) => active?.has(id)) : [], writable: (target === 'text' ? !submittingText && active?.textWritable() : active?.writable()) ?? false };
}

function bound(sessionId: unknown): PluginComposerPort {
  if (typeof sessionId !== 'string' || !sessionId || !active || active.sessionId() !== sessionId) throw new Error('插件输入请求不属于当前活动会话');
  return active;
}

// 在途 Promise 只是输入端口的互斥与回执等待；去重、接纳和排队事实仍由 daemon 拥有。
export function submitPluginComposerText(input: PluginComposerTextInput, admit: () => Promise<void>): Promise<PluginComposerTextReceipt> {
  const port = bound(input.sessionId);
  if (typeof input.submissionId !== 'string' || !input.submissionId.trim() || input.submissionId.trim() !== input.submissionId
    || /[\u0000-\u001f\u007f]/.test(input.submissionId) || input.submissionId.length > 128
    || typeof input.text !== 'string' || !input.text.trim() || input.text.length > MAX_COMPOSER_TEXT_CHARS) throw new Error(i18n.t('input.pluginTextInvalid'));
  if (submittingText) {
    if (submittingText.port === port && submittingText.input.sessionId === input.sessionId
      && submittingText.input.submissionId === input.submissionId && submittingText.input.text === input.text) return submittingText.result;
    throw new Error(i18n.t('input.pluginTextPending'));
  }
  const captured = { ...input };
  const authorize = async () => {
    await admit();
    if (bound(captured.sessionId) !== port) throw new Error(i18n.t('input.pluginTextUnavailable'));
  };
  const pending: TextSubmission = { port, input: captured,
    result: Promise.resolve().then(() => port.submitText(captured, authorize)).finally(() => {
      if (submittingText === pending) { submittingText = undefined; notifyPluginComposerChanged(); }
    }),
  };
  submittingText = pending;
  notifyPluginComposerChanged();
  return pending.result;
}

/** 只观察现有原生提交回执；失联或停止等待都不能推断请求未接纳。 */
export function observeComposerSubmission(submissionId: string, timeoutMs = 30_000) {
  let finish: (status: PluginComposerTextReceipt['status']) => void;
  const result = new Promise<PluginComposerTextReceipt>(resolve => {
    finish = status => { window.removeEventListener('magi:sessionTurnSubmissionSettled', receive); clearTimeout(timer); resolve({ submissionId, status }); };
  });
  const receive = (event: Event) => {
    const detail = (event as CustomEvent<{ requestId?: string; status?: string }>).detail;
    if (detail?.requestId === submissionId) finish(detail.status === 'accepted' ? 'accepted' : 'unknown');
  };
  const timer = setTimeout(() => finish('unknown'), timeoutMs);
  window.addEventListener('magi:sessionTurnSubmissionSettled', receive);
  return { result, dispose() { finish('unknown'); } };
}

async function decodeImage(input: unknown): Promise<PluginComposerImage> {
  const value = input as Partial<PluginComposerImage> | null;
  if (!value || typeof value.dataUrl !== 'string' || typeof value.name !== 'string' || !value.name.trim() || value.name.length > 256) throw new Error('图片草稿无效');
  const match = /^data:([^;,]+);base64,([A-Za-z0-9+/]*={0,2})$/i.exec(value.dataUrl);
  if (!match || !isSupportedComposerDataUrl(value.dataUrl)) throw new Error('图片草稿格式无效或超过大小上限');
  if (match[2].length > Math.ceil(MAX_COMPOSER_IMAGE_BYTES / 3) * 4) throw new Error('图片草稿格式无效或超过大小上限');
  const binary = atob(match[2]);
  if (!binary.length || btoa(binary) !== match[2] || value.size !== binary.length || binary.length > MAX_COMPOSER_IMAGE_BYTES) throw new Error('图片草稿编码或大小不匹配');
  const image = new Image();
  image.src = value.dataUrl;
  let timeout: ReturnType<typeof setTimeout> | undefined;
  let width = 0, height = 0;
  try {
    await Promise.race([image.decode(), new Promise<never>((_, reject) => {
      timeout = setTimeout(() => reject(new Error('图片草稿解码超时')), 10_000);
    })]);
    width = image.naturalWidth; height = image.naturalHeight;
  } finally { clearTimeout(timeout); image.src = ''; }
  if (!width || !height || width * height > 64_000_000) throw new Error('图片草稿尺寸无效或超过上限');
  return { dataUrl: value.dataUrl, name: value.name, size: binary.length };
}

export async function addPluginComposerImages(sessionId: unknown, input: unknown, admit: () => Promise<void>): Promise<string[] | null> {
  const port = bound(sessionId);
  if (!Array.isArray(input) || !input.length || input.length > 5) throw new Error('图片草稿数量无效');
  const finish = port.beginRead(input.length);
  if (!finish) return null;
  try {
    const images: PluginComposerImage[] = [];
    for (const value of input) images.push(await decodeImage(value));
    await admit();
    // 解码期间可能换会话、进入队列编辑或卸载输入区；提交时重新校验同一个端口。
    if (bound(sessionId) !== port) throw new Error('输入框实例已更换');
    finish();
    return port.add(images);
  } finally { finish(); }
}

export function pluginComposerHas(sessionId: unknown, id: string): boolean { return bound(sessionId).has(id); }
export function removePluginComposerImage(sessionId: unknown, id: string): boolean { return bound(sessionId).remove(id); }
export function focusPluginComposer(sessionId: unknown): void { bound(sessionId).focus(); }
export function notifyPluginComposer(sessionId: unknown, level: unknown, message: unknown): void {
  if (!['info', 'warning', 'error', 'success'].includes(String(level)) || typeof message !== 'string' || !message.trim() || message.length > 4096) throw new Error('插件通知无效');
  bound(sessionId).notify(level as 'info' | 'warning' | 'error' | 'success', message);
}
