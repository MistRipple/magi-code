import type { PluginComposerSnapshot, PluginComposerTarget, PluginComposerTextInput } from '../shared/app-server-protocol.generated';
import { addPluginComposerImages, focusPluginComposer, notifyPluginComposer, pluginComposerHas, pluginComposerSnapshot, removePluginComposerImage, submitPluginComposerText } from './plugin-composer';
import { i18n } from '../stores/i18n.svelte';

// 只记录当前隔离文档获授的图片身份，不复制 InputArea 的图片、文本或草稿状态。
export function createPluginComposerBridge(admit: (target: PluginComposerTarget) => Promise<void>, isCurrent: () => boolean, pluginId: string) {
  const owned = new Map<string, string>();
  const snapshot = (target: PluginComposerTarget = 'images'): PluginComposerSnapshot => {
    const current = pluginComposerSnapshot([]);
    return pluginComposerSnapshot([...owned].filter(([, session]) => session === current.sessionId).map(([id]) => id), target);
  };
  const ensureCurrent = () => { if (!isCurrent()) throw new Error('插件页面身份已失效'); };
  const authorize = async (target: PluginComposerTarget = 'images') => { ensureCurrent(); await admit(target); ensureCurrent(); };
  return {
    snapshot,
    async call(operation: string, payload: { sessionId?: unknown; id?: unknown; images?: unknown; level?: unknown; message?: unknown; target?: unknown; submissionId?: unknown; text?: unknown }) {
      ensureCurrent();
      if (operation === 'composer.submitText') {
        await authorize('text');
        const input = payload as PluginComposerTextInput;
        if (typeof input.submissionId !== 'string' || !input.submissionId.trim() || input.submissionId.trim() !== input.submissionId
          || /[\u0000-\u001f\u007f]/.test(input.submissionId) || input.submissionId.length > 128
          || typeof input.sessionId !== 'string') throw new Error(i18n.t('input.pluginTextInvalid'));
        // 插件提供的是自己的操作身份，不能占用用户或其他插件的 requestId。
        const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(JSON.stringify([pluginId, input.sessionId, input.submissionId])));
        const requestId = 'plugin-composer-' + [...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, '0')).join('');
        const receipt = await submitPluginComposerText({ sessionId: input.sessionId, submissionId: requestId, text: input.text }, () => authorize('text'));
        return { ...receipt, submissionId: input.submissionId };
      }
      if (operation === 'composer.addImages') {
        await authorize();
        if (owned.size >= 512) throw new Error('当前插件页面的图片身份超过上限');
        const ids = await addPluginComposerImages(payload.sessionId, payload.images, () => authorize());
        ensureCurrent();
        for (const id of ids ?? []) owned.set(id, payload.sessionId as string);
        return { accepted: ids !== null, ids: ids ?? [], snapshot: snapshot() };
      }
      await authorize(operation === 'composer.snapshot' && payload.target === 'text' ? 'text' : 'images');
      if (operation === 'composer.snapshot') return snapshot(payload.target === 'text' ? 'text' : 'images');
      if (payload.sessionId !== snapshot().sessionId) throw new Error('插件输入请求不属于当前活动会话');
      if (operation === 'composer.removeImage') {
        if (typeof payload.id !== 'string' || owned.get(payload.id) !== payload.sessionId) throw new Error('图片不属于当前插件文档');
        const removed = removePluginComposerImage(payload.sessionId, payload.id);
        if (removed || !pluginComposerHas(payload.sessionId, payload.id)) owned.delete(payload.id);
        return { removed, snapshot: snapshot() };
      }
      if (operation === 'composer.focus') { focusPluginComposer(payload.sessionId); return null; }
      if (operation === 'composer.notify') { notifyPluginComposer(payload.sessionId, payload.level, payload.message); return null; }
      throw new Error('未知的插件输入操作');
    },
  };
}
