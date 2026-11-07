import type { PluginComposerSnapshot } from '../shared/app-server-protocol.generated';
import { addPluginComposerImages, focusPluginComposer, notifyPluginComposer, pluginComposerHas, pluginComposerSnapshot, removePluginComposerImage } from './plugin-composer';

// 只记录当前隔离文档获授的图片身份，不复制 InputArea 的图片、文本或草稿状态。
export function createPluginComposerBridge(admit: () => Promise<void>, isCurrent: () => boolean) {
  const owned = new Map<string, string>();
  const snapshot = (): PluginComposerSnapshot => {
    const current = pluginComposerSnapshot([]);
    return pluginComposerSnapshot([...owned].filter(([, session]) => session === current.sessionId).map(([id]) => id));
  };
  const ensureCurrent = () => { if (!isCurrent()) throw new Error('插件页面身份已失效'); };
  const authorize = async () => { ensureCurrent(); await admit(); ensureCurrent(); };
  return {
    snapshot,
    async call(operation: string, payload: { sessionId?: unknown; id?: unknown; images?: unknown; level?: unknown; message?: unknown }) {
      ensureCurrent();
      if (operation === 'composer.addImages') {
        await authorize();
        if (owned.size >= 512) throw new Error('当前插件页面的图片身份超过上限');
        const ids = await addPluginComposerImages(payload.sessionId, payload.images, authorize);
        ensureCurrent();
        for (const id of ids ?? []) owned.set(id, payload.sessionId as string);
        return { accepted: ids !== null, ids: ids ?? [], snapshot: snapshot() };
      }
      await authorize();
      if (operation === 'composer.snapshot') return snapshot();
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
