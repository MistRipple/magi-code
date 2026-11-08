// 仅上游模块的 node:fs 导入经过此外观。profile 之外仍执行原生文件操作。
import fs from 'node:fs';
import { Readable } from 'node:stream';
import { virtualPath, profileRealpath, profileRead, profileWrite, profileRemove, profileRename, profileStat, profileList, isProfileDescriptor, profileOpen, profileFstat, profileReadDescriptor, profileReadRemaining, profileClose } from './profile.mjs';
export * from 'node:fs';
const denied = () => { throw Object.assign(new Error('此文件操作不能直接修改 daemon 的 profile 投影'), { code: 'EROFS' }); };
export const readFileSync = (path, ...args) => isProfileDescriptor(path) ? profileReadRemaining(path, ...args) : virtualPath(path) ? profileRead(path, ...args) : fs.readFileSync(path, ...args);
export const writeFileSync = (path, ...args) => profileWrite(path, ...args) || fs.writeFileSync(path, ...args);
export const existsSync = (path) => { if (!virtualPath(path)) return fs.existsSync(path); try { return Boolean(profileStat(path)); } catch (error) { if (error.code === 'ENOENT') return false; throw error; } };
export const statSync = (path, ...args) => virtualPath(path) ? profileStat(path) : fs.statSync(path, ...args);
export const lstatSync = (path, ...args) => virtualPath(path) ? profileStat(path) : fs.lstatSync(path, ...args);
export const readdirSync = (path, ...args) => virtualPath(path) ? profileList(path, ...args) : fs.readdirSync(path, ...args);
export const rmSync = (path, ...args) => profileRemove(path) || fs.rmSync(path, ...args);
export const unlinkSync = (path) => profileRemove(path) || fs.unlinkSync(path);
export const renameSync = (from, to) => profileRename(from, to) || fs.renameSync(from, to);
export const copyFileSync = (from, to, mode = 0) => {
  if (!virtualPath(from) && !virtualPath(to)) return fs.copyFileSync(from, to, mode);
  if (mode & ~(fs.constants.COPYFILE_EXCL | fs.constants.COPYFILE_FICLONE | fs.constants.COPYFILE_FICLONE_FORCE)) throw new RangeError('copyFile mode 无效');
  if (mode & fs.constants.COPYFILE_FICLONE_FORCE) throw Object.assign(new Error('profile 快照不支持强制 reflink 复制'), { code: 'ENOTSUP' });
  if (mode & fs.constants.COPYFILE_EXCL && existsSync(to)) throw Object.assign(new Error('复制目标已存在'), { code: 'EEXIST' });
  // 复制不同于原子移动：导出快照或从真实备份形成提案，都经过相同读写准入。
  // 不可变原包目标仍由 profileWrite 拒绝；不会向物理安装树写入。
  writeFileSync(to, readFileSync(from), { flag: mode & fs.constants.COPYFILE_EXCL ? 'wx' : 'w' });
};
export const mkdirSync = (path, ...args) => virtualPath(path) ? (profileStat(path).isDirectory() ? undefined : denied()) : fs.mkdirSync(path, ...args);
export const realpathSync = (path, ...args) => {
  if (!virtualPath(path)) return fs.realpathSync(path, ...args);
  const result = profileRealpath(path);
  return (typeof args[0] === 'string' ? args[0] : args[0]?.encoding) === 'buffer' ? Buffer.from(result) : result;
};
realpathSync.native = (path, ...args) => virtualPath(path) ? realpathSync(path, ...args) : fs.realpathSync.native(path, ...args);
export const accessSync = (path, mode = 0) => virtualPath(path) ? (mode & fs.constants.W_OK ? denied() : profileStat(path), undefined) : fs.accessSync(path, mode);
export const openSync = (path, ...args) => virtualPath(path) ? profileOpen(path, ...args) : fs.openSync(path, ...args);
export const fstatSync = (fd, ...args) => isProfileDescriptor(fd) ? profileFstat(fd, ...args) : fs.fstatSync(fd, ...args);
export const closeSync = (fd) => isProfileDescriptor(fd) ? profileClose(fd) : fs.closeSync(fd);
export const readSync = (fd, buffer, offset, length, position) => {
  if (!isProfileDescriptor(fd)) return fs.readSync(fd, buffer, offset, length, position);
  if (offset && typeof offset === 'object') {
    const options = offset;
    offset = options.offset ?? 0;
    length = options.length ?? (buffer.byteLength - offset);
    position = options.position ?? null;
  }
  return profileReadDescriptor(fd, buffer, offset, length, position);
};
export const watch = (path, ...args) => virtualPath(path) ? denied() : fs.watch(path, ...args);
export const createReadStream = (path, ...args) => virtualPath(path) ? Readable.from([profileRead(path)]) : fs.createReadStream(path, ...args);
function virtualDir(path, options) {
  const entries = profileList(path, { ...(options ?? {}), withFileTypes: true });
  let index = 0, closed = false;
  const checkOpen = () => { if (closed) throw Object.assign(new Error('Directory handle was closed'), { code: 'ERR_DIR_CLOSED' }); };
  const invoke = (operation, done) => {
    if (done === undefined) return Promise.resolve().then(operation);
    if (typeof done !== 'function') throw new TypeError('缺少回调');
    queueMicrotask(() => { let value; try { value = operation(); } catch (error) { done(error); return; } done(null, value); });
  };
  return {
    path: String(path),
    read(done) { return invoke(() => this.readSync(), done); },
    readSync() { checkOpen(); return index < entries.length ? entries[index++] : null; },
    close(done) { return invoke(() => this.closeSync(), done); },
    closeSync() { checkOpen(); closed = true; },
    async *[Symbol.asyncIterator]() {
      try { let entry; while ((entry = await this.read()) !== null) yield entry; }
      finally { if (!closed) this.closeSync(); }
    },
  };
}
export const opendirSync = (path, options) => virtualPath(path) ? virtualDir(path, options) : fs.opendirSync(path, options);
const callback = (name, fn) => (path, ...args) => {
  if (!virtualPath(path) && !isProfileDescriptor(path) && !(['rename', 'copyFile'].includes(name) && virtualPath(args[0]))) return fs[name](path, ...args);
  const done = args.pop();
  if (typeof done !== 'function') throw new TypeError('缺少回调');
  queueMicrotask(() => { let value; try { value = fn(path, ...args); } catch (error) { done(error); return; } done(null, value); });
};
export const readFile = callback('readFile', readFileSync);
export const writeFile = callback('writeFile', writeFileSync);
export const readdir = callback('readdir', readdirSync);
export const stat = callback('stat', statSync);
export const lstat = callback('lstat', lstatSync);
export const mkdir = callback('mkdir', mkdirSync);
export const unlink = callback('unlink', unlinkSync);
export const rm = callback('rm', rmSync);
export const rename = callback('rename', renameSync);
export const copyFile = callback('copyFile', copyFileSync);
export const access = callback('access', accessSync);
export const realpath = callback('realpath', realpathSync);
realpath.native = callback('realpath', realpathSync.native);
export const open = callback('open', openSync);
export const fstat = callback('fstat', fstatSync);
export const close = callback('close', closeSync);
export const read = (fd, buffer, ...args) => {
  if (!isProfileDescriptor(fd)) return fs.read(fd, buffer, ...args);
  const done = args.pop();
  if (typeof done !== 'function') throw new TypeError('缺少回调');
  queueMicrotask(() => { let bytes; try { bytes = readSync(fd, buffer, ...args); } catch (error) { done(error); return; } done(null, bytes, buffer); });
};
const promise = (name, fn) => async (path, ...args) => virtualPath(path) || (['rename', 'copyFile'].includes(name) && virtualPath(args[0])) ? fn(path, ...args) : fs.promises[name](path, ...args);
export const promises = {
  ...fs.promises,
  readFile: promise('readFile', readFileSync), writeFile: promise('writeFile', writeFileSync),
  readdir: promise('readdir', readdirSync), stat: promise('stat', statSync), lstat: promise('lstat', lstatSync),
  mkdir: promise('mkdir', mkdirSync), rm: promise('rm', rmSync), unlink: promise('unlink', unlinkSync),
  rename: promise('rename', renameSync), access: promise('access', accessSync), realpath: promise('realpath', realpathSync),
  copyFile: promise('copyFile', copyFileSync),
  opendir: async (path, options) => virtualPath(path) ? virtualDir(path, options) : fs.promises.opendir(path, options),
  open: async (path, ...args) => {
    if (!virtualPath(path)) return fs.promises.open(path, ...args);
    const fd = openSync(path, ...args);
    let closed = false;
    const handle = {
      get fd() { return closed ? -1 : fd; },
      async stat(options) { return fstatSync(fd, options); },
      async read(buffer, ...args) { return { bytesRead: readSync(fd, buffer, ...args), buffer }; },
      async readFile(options) { return profileReadRemaining(fd, options); },
      async close() { if (!closed) { closeSync(fd); closed = true; } },
      async [Symbol.asyncDispose]() { await handle.close(); },
    };
    return handle;
  },
};
export const opendir = callback('opendir', opendirSync);
export default { ...fs, readFileSync, writeFileSync, existsSync, statSync, lstatSync, readdirSync, rmSync, unlinkSync, renameSync, copyFileSync, mkdirSync, realpathSync, accessSync, openSync, fstatSync, closeSync, readSync, opendirSync, watch, createReadStream, readFile, writeFile, readdir, stat, lstat, mkdir, unlink, rm, rename, copyFile, access, realpath, open, fstat, close, read, opendir, promises };
