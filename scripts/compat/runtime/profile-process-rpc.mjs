// 同步进程的异步 RPC 入口，与可被 CommonJS 导入的运行库分离。
import { callProfileProcess } from './profile-process-call.mjs';
import { readFileSync } from 'node:fs';
try { const { method, input } = JSON.parse(readFileSync(0, 'utf8')); process.stdout.write(JSON.stringify(await callProfileProcess(method, input) ?? null)); }
catch (error) { process.stderr.write(error.message); process.exitCode = 1; }
