import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

await withGoldenViteServer(async (server) => {
  const { confirmAction, chooseAction, settleConfirm, confirmDialogState } = await server.ssrLoadModule(
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
    settleConfirm('confirm');
    assert.equal(await pending, true);
    assert.equal(confirmDialogState.request, null);
  }
  {
    const pending = confirmAction(request('b'));
    settleConfirm('cancel');
    assert.equal(await pending, false);
    assert.equal(confirmDialogState.request, null);
  }

  // 同一时间只有一个请求：新请求会让没有答复的旧请求按「取消」结束，并显示新的内容。
  {
    const first = confirmAction(request('first'));
    const second = confirmAction(request('second'));
    assert.equal(await first, false);
    assert.equal(confirmDialogState.request?.message, 'second');
    settleConfirm('confirm');
    assert.equal(await second, true);
  }

  // 没有请求时答复是空操作。
  settleConfirm('confirm');
  assert.equal(confirmDialogState.request, null);

  // 第二动作：三个出口各自返回对应结果，Esc / 遮罩等同 cancel，不会误触发动作。
  for (const choice of ['confirm', 'secondary', 'cancel']) {
    const pending = chooseAction({ ...request('c'), secondaryLabel: '另存为' });
    assert.equal(confirmDialogState.request?.secondaryLabel, '另存为');
    settleConfirm(choice);
    assert.equal(await pending, choice);
  }
});

console.log('confirm dialog golden passed');
