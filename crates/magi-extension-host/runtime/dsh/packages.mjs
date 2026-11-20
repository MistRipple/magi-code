// Installation admission only: never imports or executes a plugin module.
import { createHash } from 'node:crypto';
import { readFile, realpath, stat } from 'node:fs/promises';
import { isAbsolute, join, relative, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import semver from 'semver';

const baseline = JSON.parse(await readFile(new URL('./package.json', import.meta.url), 'utf8'));
export const runtimeVersion = baseline.dshVersion;
if (!semver.valid(runtimeVersion)) throw new Error('Missing locked DSH runtime version');

function object(value, field) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${field} must be an object`);
  return value;
}

/** DSH app-boot requirement semantics, without its optional version exemptions. */
export function checkRuntimeRequirements(manifest, version = runtimeVersion) {
  if (!semver.valid(version)) throw new Error('Invalid DSH runtime version');
  object(manifest, 'Plugin manifest');
  const engines = Object.hasOwn(manifest, 'engines') ? object(manifest.engines, 'engines') : {};
  const requirements = {};
  const satisfies = range => range.trim() !== '' && semver.satisfies(version, range, { includePrerelease: true });
  if (Object.hasOwn(engines, 'dsh')) {
    if (typeof engines.dsh !== 'string') throw new Error('engines.dsh must be a string');
    if (!satisfies(engines.dsh)) requirements['engines.dsh'] = engines.dsh;
  } else if (Object.hasOwn(manifest, 'peerDependencies')) {
    for (const [name, range] of Object.entries(object(manifest.peerDependencies, 'peerDependencies'))) {
      if (typeof range !== 'string') throw new Error(`peerDependencies[${name}] must be a string`);
      if (name !== '@deepseek-ai/dsh' && !name.startsWith('@deepseek-ai/dsh-')) continue;
      const requirement = ['workspace:^', 'workspace:~', 'workspace:*'].includes(range) ? version : range;
      if (!satisfies(requirement)) requirements[name] = range;
    }
  }
  if (Object.keys(requirements).length) {
    throw new Error(`Plugin ${manifest.name}@${manifest.version} requires ${JSON.stringify(requirements)}; DSH baseline is ${version}`);
  }
}

/** Native ESM import conditions, in the real installation project, with no require fallback. */
export async function inspectOriginalPackage({ projectDirectory, directory, name, version, manifestSha256 }) {
  if (!process.execArgv.includes('--experimental-import-meta-resolve')) {
    throw new Error('Package inspection requires parent-aware native ESM resolution');
  }
  if (typeof projectDirectory !== 'string' || !isAbsolute(projectDirectory)
    || typeof directory !== 'string' || !isAbsolute(directory)
    || typeof name !== 'string' || name.length > 214 || !/^(?:@[a-z0-9~-][a-z0-9._~-]*\/)?[a-z0-9~-][a-z0-9._~-]*$/.test(name)
    || !semver.valid(version) || !/^[a-f0-9]{64}$/.test(manifestSha256)) throw new Error('Invalid original package identity');
  const packageRoot = await realpath(directory);
  const installedRoot = await realpath(join(projectDirectory, 'node_modules', name));
  if (installedRoot !== packageRoot) throw new Error('Package is not installed in the selected loading project');
  const file = join(packageRoot, 'package.json');
  const metadata = await stat(file);
  if (!metadata.isFile() || metadata.size > 1024 * 1024) throw new Error('Invalid original package manifest');
  const bytes = await readFile(file);
  if (bytes.length > 1024 * 1024 || createHash('sha256').update(bytes).digest('hex') !== manifestSha256) {
    throw new Error('Original package manifest changed after preparation');
  }
  const manifest = object(JSON.parse(bytes), 'Plugin manifest');
  if (manifest.name !== name || manifest.version !== version) throw new Error('Original package identity mismatch');
  checkRuntimeRequirements(manifest);
  const resolved = import.meta.resolve(name, pathToFileURL(join(projectDirectory, 'package.json')).href);
  const entry = await realpath(fileURLToPath(resolved));
  const inside = relative(packageRoot, entry);
  if (!inside || isAbsolute(inside) || inside === '..' || inside.startsWith(`..${sep}`) || !(await stat(entry)).isFile()) {
    throw new Error('Resolved backend entry must be an original package file');
  }
  const sdkModules = {};
  const dependencies = { ...manifest.peerDependencies, ...manifest.dependencies };
  for (const dependency of Object.keys(dependencies)) {
    if (!dependency.startsWith('@deepseek-ai/') && dependency !== '@earendil-works/pi-ai') continue;
    const optional = !Object.hasOwn(manifest.dependencies ?? {}, dependency)
      && manifest.peerDependenciesMeta?.[dependency]?.optional === true;
    let pluginModule;
    try {
      pluginModule = import.meta.resolve(dependency, pathToFileURL(entry).href);
    } catch (error) {
      if (optional && error.code === 'ERR_MODULE_NOT_FOUND') continue;
      throw error;
    }
    const hostModule = import.meta.resolve(dependency);
    if (await realpath(fileURLToPath(pluginModule)) !== await realpath(fileURLToPath(hostModule))) {
      throw new Error(`Plugin resolves a separate SDK module: ${dependency}`);
    }
    sdkModules[dependency] = hostModule;
  }
  return { name, version, manifestSha256, entry, runtimeVersion,
    clientDeclared: Object.hasOwn(manifest.exports ?? {}, './client'), sdkModules };
}
