import childProcess from 'node:child_process';
import { profileSpawn } from './profile.mjs';
export * from 'node:child_process';
export const spawn = (file, args, options) => profileSpawn(file, args, options) ?? childProcess.spawn(file, args, options);
export default { ...childProcess, spawn };
