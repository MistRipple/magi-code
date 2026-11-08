import { promises } from './profile-fs.mjs';
export * from 'node:fs/promises';
export const { readFile, writeFile, readdir, stat, lstat, mkdir, rm, unlink, rename, copyFile, access, realpath, opendir, open } = promises;
export default promises;
