import childProcess from 'node:child_process';
import { profileSpawn } from './profile.mjs';
import { applyProfileSnapshot } from './profile.mjs';
import { writeSync } from 'node:fs';
import { packageManagerScript } from './profile-process-host.mjs';
import { callProfileProcessSync } from './profile-process-call.mjs';
export * from 'node:child_process';
export const spawn = (file, args, options) => profileSpawn(file, args, options) ?? childProcess.spawn(file, args, options);
export const spawnSync = (file, args = [], options = {}) => {
  if (file !== process.execPath || args[0] !== packageManagerScript || !process.env.MAGI_PROFILE_PROCESS_TOKEN) return childProcess.spawnSync(file, args, options);
  let stdout = Buffer.alloc(0), stderr = Buffer.alloc(0), status = 1, processId;
  try {
    const { value: reply, pid } = callProfileProcessSync('install', args.slice(1), options.timeout);
    processId = pid;
    applyProfileSnapshot(undefined, reply.snapshot);
    if (reply.result.application !== 'applied') throw new Error(reply.result.error?.message ?? `安装结果：${reply.result.application}`);
    stdout = Buffer.from('Magi daemon 已提交插件管理操作\n'); status = 0;
  } catch (error) { stderr = Buffer.from(error.message); }
  const output = [null, stdout, stderr];
  for (const [index, bytes] of [[1, stdout], [2, stderr]]) {
    const target = Array.isArray(options.stdio) ? options.stdio[index] : options.stdio;
    if (typeof target === 'number' || target === 'inherit') { writeSync(typeof target === 'number' ? target : index, bytes); output[index] = null; }
    else if (target === 'ignore') output[index] = null;
    else if (options.encoding && options.encoding !== 'buffer') output[index] = bytes.toString(options.encoding);
  }
  return { pid: processId, status, signal: null, error: undefined, stdout: output[1], stderr: output[2], output };
};
export default { ...childProcess, spawn, spawnSync };
