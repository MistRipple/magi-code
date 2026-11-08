// 辅助进程到所属扩展实例的有界私有调用；没有 daemon 凭据或独立安装事实。
import { connect } from 'node:net';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
export const MAX_PROCESS_FRAME = 4 * 1024 * 1024;
export function callProfileProcess(method, input) {
  return new Promise((resolve, reject) => {
    const socket = connect(process.env.MAGI_PROFILE_PROCESS_ENDPOINT);
    const request = JSON.stringify({ token: process.env.MAGI_PROFILE_PROCESS_TOKEN, method, input }) + '\n';
    if (Buffer.byteLength(request) > MAX_PROCESS_FRAME) { socket.destroy(); reject(new Error('插件辅助进程请求超过大小上限')); return; }
    let data = Buffer.alloc(0);
    socket.setTimeout(900_000, () => socket.destroy(new Error('插件辅助进程调用超时')));
    socket.on('error', reject);
    socket.on('connect', () => socket.write(request));
    socket.on('data', chunk => {
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_PROCESS_FRAME) return socket.destroy(new Error('插件辅助进程响应超过大小上限'));
      const end = data.indexOf(10);
      if (end < 0) return;
      try {
        const reply = JSON.parse(data.subarray(0, end));
        if (reply.error) reject(Object.assign(new Error(reply.error.message), { code: reply.error.code }));
        else resolve(reply.value);
      } catch (error) { reject(error); }
      socket.end();
    });
    socket.on('end', () => { if (!data.includes(10)) reject(new Error('插件辅助进程调用连接已关闭')); });
  });
}
export function callProfileProcessSync(method, input, timeout = 900_000) {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('profile-process-rpc.mjs', import.meta.url))],
    { input: JSON.stringify({ method, input }), timeout, maxBuffer: MAX_PROCESS_FRAME, encoding: 'utf8' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr || `辅助进程调用退出：${result.status}`);
  return { value: JSON.parse(result.stdout), pid: result.pid };
}
