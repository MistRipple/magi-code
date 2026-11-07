// 只保存当前文档的 File 草稿与已接纳身份映射。已挂载草稿的唯一事实归
// InputArea；session/attachment 快照都来自其真实回执，不在这里创建会话。
export function createSessionComposerServices(composer) {
  const drafts = new Map(), mounted = new Map(), scopes = new WeakMap();
  const listeners = new Set();
  let current = composer.getSnapshot();
  let stopped = false;
  let list = Object.freeze({ ids: [], byId: {} });
  const update = () => {
    current = composer.getSnapshot();
    for (const [id, binding] of mounted) if (binding.sessionId === current.sessionId && !current.attachmentIds.includes(binding.attachmentId)) mounted.delete(id);
    if ((list.ids[0] ?? null) !== current.sessionId) list = Object.freeze({
      ids: Object.freeze(current.sessionId ? [current.sessionId] : []),
      byId: Object.freeze(current.sessionId ? { [current.sessionId]: Object.freeze({ id: current.sessionId, retainedBy: Object.freeze({ mainView: 1 }) }) } : {}),
    });
    for (const listener of [...listeners]) listener();
  };
  const unsubscribe = composer.subscribe(update);
  update();
  const sessionOf = (scope) => {
    const session = scopes.get(scope);
    if (stopped || !session || session !== current.sessionId) throw new Error('输入端口不属于当前活动会话或页面已失效');
    return session;
  };
  const rejectSync = () => { throw new Error('此同步宿主调用必须先由翻译器转换为等待真实回执的业务调用'); };
  const inputFor = (scope) => {
    sessionOf(scope);
    return Object.freeze({
      get snapshot() { const sessionId = sessionOf(scope); return Object.freeze({ attachmentIds: [...mounted].filter(([, binding]) => binding.sessionId === sessionId).map(([id]) => id) }); },
      addAttachments: rejectSync, removeAttachment: rejectSync, focus: rejectSync, notify: rejectSync,
      async addAttachmentsAsync(ids) {
        const sessionId = sessionOf(scope);
        if (!Array.isArray(ids) || !ids.length || ids.length > 5 || new Set(ids).size !== ids.length) throw new Error('草稿批次身份无效');
        const pending = ids.map((id) => drafts.get(id));
        if (pending.some((draft, index) => !draft || draft.sessionId !== sessionId || mounted.has(ids[index]))) throw new Error('草稿不属于当前文档与会话，或已经挂载');
        const result = await composer.addImages(pending.map((draft) => draft.file), sessionId);
        if (stopped) throw new Error('插件页面已失效');
        if (!result.accepted) return false;
        if (result.ids.length !== ids.length) throw new Error('宿主草稿回执身份数量不匹配');
        ids.forEach((id, index) => mounted.set(id, { sessionId, attachmentId: result.ids[index] }));
        return true;
      },
      async removeAttachmentAsync(id) {
        const sessionId = sessionOf(scope), binding = mounted.get(id);
        if (!binding || binding.sessionId !== sessionId) return false;
        const result = await composer.removeImage(binding.attachmentId, sessionId);
        if (result.removed || !result.snapshot.attachmentIds.includes(binding.attachmentId)) mounted.delete(id);
        return result.removed;
      },
      focusAsync() { return composer.focus(sessionOf(scope)); },
      notifyAsync(level, message) { return composer.notify(level, message, sessionOf(scope)); },
    });
  };
  const sessions = Object.freeze({
    list: Object.freeze({ getSnapshot: () => list, subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); } }),
    scope(sessionId) {
      if (stopped || !sessionId || sessionId !== current.sessionId) return undefined;
      const token = Object.freeze({}); scopes.set(token, sessionId); return token;
    },
  });
  const conversation = Object.freeze({
    input: Object.freeze({ for: inputFor }),
    createDrafts(sessionId, files) {
      if (stopped || !sessionId || sessionId !== current.sessionId || !Array.isArray(files) || !files.length || files.length > 5 || drafts.size + files.length > 64
        || [...drafts.values()].reduce((bytes, draft) => bytes + draft.file.size, 0) + files.reduce((bytes, file) => bytes + (file?.size ?? 0), 0) > 32 * 1024 * 1024
        || files.some((file) => !file || typeof file.arrayBuffer !== 'function' || !file.type?.startsWith('image/') || !file.size || file.size > 10 * 1024 * 1024)) throw new Error('草稿文件、会话或容量无效');
      const result = files.map((file) => Object.freeze({ id: `magi-draft-${crypto.randomUUID()}`, sessionId, file, name: file.name, mediaType: file.type, bytes: file.size }));
      result.forEach((draft) => drafts.set(draft.id, draft));
      return result;
    },
    resolveDraftAttachments(ids) { return ids.map((id) => drafts.get(id)).filter(Boolean); },
    releaseDraftAttachment(id) {
      // 释放页面的 File 引用不等于移除 InputArea 中已经接纳的图片；移除必须等待上面的宿主操作。
      drafts.delete(id);
    },
  });
  return { sessions, conversation, dispose() { stopped = true; unsubscribe(); listeners.clear(); drafts.clear(); mounted.clear(); } };
}
