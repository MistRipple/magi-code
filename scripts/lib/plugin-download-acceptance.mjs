// 在脚本自有 Main 中选择保存位置，或取消本次原生下载；不替换原包的
// Blob、链接、组件和回执。Electron 的 App session 不属于 CDP 管理的
// browser contexts，不能用默认 context 的下载设置冒充该窗口已保存。
import { mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { connectTarget, waitFor } from './electron-harness.mjs';

export async function capturePluginDownload({ inspectPort, pid }, directory, trigger, { cancel = false } = {}) {
  await mkdir(directory, { recursive: true });
  const main = await connectTarget(inspectPort, () => true, '验收实例的 Main inspector');
  let registered = false;
  try {
    if (await main.evaluate('process.pid') !== pid) throw new Error('Main inspector 不属于当前验收实例');
    await main.evaluate(`(() => {
      if (globalThis.__magiDownloadAcceptance) throw new Error('已有下载验收正在运行');
      const { BrowserWindow } = process.getBuiltinModule('module')._load('electron');
      const windows = BrowserWindow.getAllWindows();
      if (windows.length !== 1) throw new Error('验收实例应只有一个 App 窗口');
      const contents = windows[0].webContents, session = contents.session;
      const state = globalThis.__magiDownloadAcceptance = { session };
      state.listener = (event, item, sender) => {
        if (sender !== contents) throw new Error('下载不属于本次 App 窗口');
        state.filename = item.getFilename();
        item.once('done', (_event, status) => {
          state.result = { state: status, suggestedFilename: state.filename,
            receivedBytes: item.getReceivedBytes(), totalBytes: item.getTotalBytes() };
        });
        ${cancel ? 'item.cancel();' : `item.setSavePath(process.getBuiltinModule('path').join(${JSON.stringify(directory)}, state.filename));`}
      };
      session.once('will-download', state.listener);
    })()`);
    registered = true;
    await trigger();
    const result = await waitFor(() => main.evaluate('globalThis.__magiDownloadAcceptance.result'), '原包文件的原生下载终态', 15_000);
    const bytes = result.state === 'completed' ? await readFile(join(directory, result.suggestedFilename)) : null;
    return { ...result, bytes };
  } finally {
    try { if (registered) await main.evaluate(`(() => {
      const state = globalThis.__magiDownloadAcceptance;
      if (state) state.session.removeListener('will-download', state.listener);
      delete globalThis.__magiDownloadAcceptance;
    })()`); }
    finally { main.close(); }
  }
}
