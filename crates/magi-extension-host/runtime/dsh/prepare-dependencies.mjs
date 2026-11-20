// Assembly of candidate projects only. The daemon owns staging, cancellation and admission.
import { execFile } from 'node:child_process';
import { mkdir, readFile, readdir, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative } from 'node:path';
import { promisify } from 'node:util';
import semver from 'semver';
import { checkRuntimeRequirements, inspectOriginalPackage } from './packages.mjs';

const npmExec = promisify(execFile);
const packageName = /^(?:@[a-z0-9~-][a-z0-9._~-]*\/)?[a-z0-9~-][a-z0-9._~-]*$/;
const sdk = name => name.startsWith('@deepseek-ai/') || name === '@earendil-works/pi-ai';
const isDshPackage = name => name === '@deepseek-ai/dsh' || name.startsWith('@deepseek-ai/dsh-');
const satisfiesSingleton = (lockedVersion, range) => {
  if (semver.satisfies(lockedVersion, range, { includePrerelease: true })) return true;
  const min = semver.minVersion(range);
  return Boolean(min && semver.major(lockedVersion) === semver.major(min) && semver.gte(lockedVersion, min));
};
const integrityPattern = /^sha512-[A-Za-z0-9+/]{86}==$/;
const readJson = async path => JSON.parse(await readFile(path, 'utf8'));

async function verifyOriginalFiles(original, installed) {
  for (const file of await readdir(original, { recursive: true, withFileTypes: true })) {
    if (!file.isFile()) continue;
    const source = join(file.parentPath, file.name);
    const path = relative(original, source);
    const [before, after] = await Promise.all([readFile(source), readFile(join(installed, path))]);
    if (!before.equals(after)) throw new Error(`npm changed an original package file: ${path}`);
  }
}

async function prepare({ project, sdkProject, npm, registry, originals }) {
  if (![project, sdkProject, npm].every(isAbsolute) || !Array.isArray(originals) || !originals.length) {
    throw new Error('Dependency preparation requires a private project, SDK project and original packages');
  }
  const sdkLock = (await readJson(join(sdkProject, 'package-lock.json'))).packages;
  const pinned = new Map(Object.entries(sdkLock).flatMap(([location, entry]) => {
    const name = location.slice('node_modules/'.length);
    return location.startsWith('node_modules/') && packageName.test(name) && sdk(name) ? [[name, entry]] : [];
  }));
  const selected = new Map();
  const dependencies = {};
  for (const original of originals) {
    const { name, version, integrity, archive, manifest } = original;
    if (!packageName.test(name) || !semver.valid(version) || !integrityPattern.test(integrity)
      || !/^archives\/[0-9]+\.tgz$/.test(archive) || !/^originals\/[0-9]+\/package\.json$/.test(manifest)
      || pinned.has(name) || selected.has(name)) throw new Error('Invalid prepared archive identity');
    const value = await readJson(join(project, manifest));
    if (value.name !== name || value.version !== version) throw new Error('Original manifest changed during preparation');
    checkRuntimeRequirements(value);
    for (const [dependency, range] of Object.entries({ ...value.peerDependencies, ...value.optionalDependencies, ...value.dependencies })) {
      if (!sdk(dependency)) continue;
      const required = Object.hasOwn(value.dependencies ?? {}, dependency);
      const optional = !required && (Object.hasOwn(value.optionalDependencies ?? {}, dependency)
        || value.peerDependenciesMeta?.[dependency]?.optional === true);
      const locked = pinned.get(dependency);
      if (!locked) {
        if (optional) continue;
        throw new Error(`Shared SDK dependency is missing: ${dependency}`);
      }
      // DSH checks host version ahead of peer ranges, unless the dependency
      // is an ordinary requirement or belongs to Cordis/singleton lines.
      const dshPeer = !required && !Object.hasOwn(value.optionalDependencies ?? {}, dependency)
        && isDshPackage(dependency);
      const compatible = isDshPackage(dependency)
        ? semver.satisfies(locked.version, range, { includePrerelease: true })
        : satisfiesSingleton(locked.version, range);
      if (!dshPeer && !compatible) {
        throw new Error(`Shared SDK ${dependency}@${locked.version} does not satisfy ${range}`);
      }
      dependencies[dependency] = locked.version;
    }
    dependencies[name] = `file:./${archive}`;
    selected.set(name, original);
  }
  const overrides = Object.fromEntries([...pinned].map(([name, entry]) => [name, entry.version]));
  await writeFile(join(project, 'package.json'), JSON.stringify({
    name: 'magi-native-plugin-project', private: true, dependencies, overrides,
  }, null, 2), { flag: 'wx' });
  const flags = ['--ignore-scripts', '--no-audit', '--no-fund', '--bin-links=false', '--strict-peer-deps',
    '--include=prod', '--include=peer', '--include=optional', '--lockfile-version=3', '--workspaces=false', '--global=false',
    '--allow-directory=none', '--allow-git=none', '--allow-remote=none', '--allow-file=root', `--registry=${registry}`];
  const run = args => npmExec(process.execPath, [npm, ...args, ...flags], { cwd: project, maxBuffer: 1024 * 1024 });
  await run(['install', '--package-lock-only']);
  const lock = await readJson(join(project, 'package-lock.json'));
  if (lock.lockfileVersion !== 3 || !lock.packages?.['']) throw new Error('npm returned no dependency lock');
  const shared = [];
  const lockedOriginals = new Set();
  for (const [location, entry] of Object.entries(lock.packages)) {
    if (!location) continue;
    const parts = location.split(/(?:^|\/)node_modules\//).slice(1);
    const name = parts.at(-1);
    if (!parts.length || !parts.every(part => packageName.test(part)) || entry.link
      || !semver.valid(entry.version) || !integrityPattern.test(entry.integrity)) throw new Error(`Invalid locked dependency: ${location}`);
    const original = parts.length === 1 && selected.get(name);
    if (original) {
      if (entry.version !== original.version || entry.integrity !== original.integrity || entry.resolved !== `file:${original.archive}`) {
        throw new Error(`npm changed original archive identity: ${name}`);
      }
      lockedOriginals.add(name);
    } else {
      const url = new URL(entry.resolved);
      if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password) throw new Error(`Invalid registry URL: ${name}`);
    }
    if (!original && sdk(name)) {
      const pin = pinned.get(name);
      if (parts.length !== 1 || !pin || pin.version !== entry.version || pin.integrity !== entry.integrity) {
        throw new Error(`Dependency graph resolved a separate SDK module: ${location}`);
      }
      const directory = await realpath(join(sdkProject, location));
      const installed = await readJson(join(directory, 'package.json'));
      if (installed.name !== name || installed.version !== pin.version) throw new Error(`Shared SDK does not match its lock: ${name}`);
      shared.push([location, directory]);
    }
  }
  if (lockedOriginals.size !== selected.size) throw new Error('Dependency lock omitted an original package');
  await run(['ci']);
  for (const [location, directory] of shared) {
    const installed = join(project, location);
    await rm(installed, { recursive: true, force: true });
    await mkdir(dirname(installed), { recursive: true });
    await symlink(directory, installed, process.platform === 'win32' ? 'junction' : 'dir');
  }
  for (const { name, version, manifestSha256, manifest } of originals) {
    const directory = join(project, 'node_modules', name);
    await verifyOriginalFiles(dirname(join(project, manifest)), directory);
    await inspectOriginalPackage({ projectDirectory: project, directory, name, version, manifestSha256 });
  }
}

try {
  await prepare(JSON.parse(await readFile(process.argv[2], 'utf8')));
} catch (error) {
  process.stderr.write(String(error.stderr || error.message).slice(0, 8192));
  process.exitCode = 1;
}
