// 异步图片草稿端口：成功只来自宿主 InputArea 的真实回执。不会以同步 true
// 冒充 DSH addAttachments 的挂载确认；DSH 同步调用仍须单独完成语义适配。
export function createComposerClient(request) {
  let current = Object.freeze({ sessionId: null, attachmentIds: [], writable: false });
  const listeners = new Set();
  const update = (snapshot) => {
    current = Object.freeze({ sessionId: snapshot.sessionId, attachmentIds: Object.freeze([...snapshot.attachmentIds]), writable: snapshot.writable });
    for (const listener of listeners) listener();
    return current;
  };
  const refresh = async (target = 'images') => update(await request({ operation: 'composer.snapshot', target }));
  const call = async (operation, payload, expectedSessionId) => {
    const sessionId = expectedSessionId ?? current.sessionId ?? (await refresh()).sessionId;
    if (!sessionId) throw new Error('当前没有活动会话');
    const result = await request({ operation, sessionId, ...payload });
    if (result?.snapshot) update(result.snapshot);
    return result;
  };
  return {
    update,
    api: Object.freeze({
      getSnapshot: () => current,
      subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
      refresh,
      // 身份由发起页面保留；断开或未知回执后只能用同一输入显式重试。
      submitText(text, { sessionId, submissionId }) {
        return request({ operation: 'composer.submitText', text, sessionId, submissionId });
      },
      async addImages(files, expectedSessionId) {
        if (!Array.isArray(files) || !files.length || files.length > 5 || files.some((file) => !file || typeof file.arrayBuffer !== 'function' || file.size > 10 * 1024 * 1024)) throw new Error('图片草稿无效或超过大小上限');
        // 捕获读取前的会话身份；读取期间切换会话不能静默投递到新会话。
        const target = await refresh();
        if (!target.sessionId || expectedSessionId && expectedSessionId !== target.sessionId) return { accepted: false, ids: [], snapshot: target };
        const images = [];
        for (const file of files) {
          const bytes = new Uint8Array(await file.arrayBuffer());
          let binary = '';
          for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
          images.push({ dataUrl: `data:${file.type};base64,${btoa(binary)}`, name: file.name, size: bytes.length });
        }
        const result = await request({ operation: 'composer.addImages', sessionId: target.sessionId, images });
        update(result.snapshot);
        return result;
      },
      removeImage: (id, sessionId) => call('composer.removeImage', { id }, sessionId),
      focus: (sessionId) => call('composer.focus', {}, sessionId),
      notify: (level, message, sessionId) => call('composer.notify', { level, message }, sessionId),
    }),
    dispose() { listeners.clear(); },
  };
}
