import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const bridgeStates = [];
  globalThis.window = {
    magiDesktop: {
      surface: 'app',
      setBlockingOverlay: async ({ active }) => {
        bridgeStates.push(active);
      },
    },
  };
  const contract = await server.ssrLoadModule('/src/shared/desktop-overlay-contract.ts');
  const states = [];
  const stop = contract.onDesktopBlockingOverlayChange((visible) => states.push(visible));

  contract.setDesktopBlockingOverlay('settings', true);
  contract.setDesktopBlockingOverlay('nested-confirm', true);
  contract.setDesktopBlockingOverlay('settings', false);
  assert.equal(contract.desktopBlockingOverlayVisible(), true, '嵌套 DOM overlay 仍存在时不能恢复 Browser Surface');
  contract.setDesktopBlockingOverlay('nested-confirm', false);
  assert.equal(contract.desktopBlockingOverlayVisible(), false, '最后一个阻塞 overlay 关闭后才恢复 Browser Surface');
  stop();

  assert.deepEqual(states, [false, true, false], '阻塞状态只应在全局可见性真正变化时通知');
  assert.deepEqual(bridgeStates, [true, false], '原生宿主必须和全局阻塞状态使用同一组可见性转换');
  delete globalThis.window;
});

console.log('desktop overlay contract golden replay passed');
