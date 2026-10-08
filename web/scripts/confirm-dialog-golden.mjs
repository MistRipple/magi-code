import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const { confirmAction, settleConfirm, confirmDialogState } = await server.ssrLoadModule(
    '/src/stores/confirm-dialog.svelte.ts',
  );
  const request = (message) => ({
    title: '标题',
    message,
    confirmLabel: '确定',
    cancelLabel: '取消',
  });

  // 确认与取消分别把 Promise 结束为 true / false，并关闭对话框。
  {
    const pending = confirmAction(request('a'));
    assert.equal(confirmDialogState.request?.message, 'a');
    settleConfirm(true);
    assert.equal(await pending, true);
    assert.equal(confirmDialogState.request, null);
  }
  {
    const pending = confirmAction(request('b'));
    settleConfirm(false);
    assert.equal(await pending, false);
    assert.equal(confirmDialogState.request, null);
  }

  // 同一时间只有一个请求：新请求会让没有答复的旧请求按「取消」结束，并显示新的内容。
  {
    const first = confirmAction(request('first'));
    const second = confirmAction(request('second'));
    assert.equal(await first, false);
    assert.equal(confirmDialogState.request?.message, 'second');
    settleConfirm(true);
    assert.equal(await second, true);
  }

  // 没有请求时答复是空操作。
  settleConfirm(true);
  assert.equal(confirmDialogState.request, null);
});

console.log('confirm dialog golden passed');
