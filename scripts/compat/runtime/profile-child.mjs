// 原包辅助脚本直接运行；只给它同一份 profile 服务与明确声明的包管理命令。
import { resolve, dirname } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { configureProfile, runProfileProcess } from './profile.mjs';
import { installSharedResolution } from './shared-resolution.mjs';
import { callProfileProcess, callProfileProcessSync } from './profile-process-call.mjs';
import { profilePackageManager } from './profile-process-host.mjs';
const [script, ...args] = process.argv.slice(2);
const entry = resolve(script);
process.argv = [process.execPath, entry, ...args];
const manager = profilePackageManager();
process.env.DSH_PACKAGE_MANAGER_COMMAND = manager.command;
process.env.DSH_PACKAGE_MANAGER_ARGS = JSON.stringify(manager.args);
process.env.DSH_PACKAGE_MANAGER_ENV = '{}';
const releaseResolution = installSharedResolution(fileURLToPath(new URL('../', import.meta.url)), { '@magi/compat-upstream-root': dirname(entry) });
const release = configureProfile({ plugins: {
  list: input => callProfileProcess('list', input),
  compatibility: () => callProfileProcess('compatibility'),
  setEnabled: (...input) => callProfileProcess('setEnabled', input),
  installBundle: input => callProfileProcess('installBundle', input),
  cancelInstall: id => callProfileProcess('cancelInstall', id),
} }, { home: process.env.MAGI_PROFILE_PROCESS_HOME, registry: process.env.MAGI_PROFILE_PROCESS_REGISTRY });
await runProfileProcess(() => import(pathToFileURL(entry).href), proposal => {
  try { if (proposal.files.length) callProfileProcessSync('commit', proposal); }
  catch (error) { console.error(`插件辅助进程提案未提交：${error.message}`); process.exitCode = 1; }
  finally { release(); releaseResolution.deregister(); }
});
