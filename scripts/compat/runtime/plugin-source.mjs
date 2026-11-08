// 上游输入与 daemon 来源投影的唯一 DSH 转换入口；不拥有安装或版本状态。
import semver from 'semver';

export const NPM_NAME = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/;
const REPOSITORY = /^[a-z0-9_.-]+\/[a-z0-9_.-]+$/i;

export function parseBundleSpec(spec) {
  if (typeof spec !== 'string' || !spec || spec.length > 512 || /[\x00-\x1f\x7f]/.test(spec)) throw new Error('插件来源无效');
  let github;
  if (spec.startsWith('github:')) github = spec.slice(7);
  else if (/^(?:git\+)?https:\/\/github\.com\//i.test(spec)) {
    const url = new URL(spec.replace(/^git\+/, ''));
    if (url.username || url.password || url.search || url.port) throw new Error('GitHub 来源不能包含凭据、端口或查询参数');
    github = url.pathname.slice(1) + url.hash;
  } else if (REPOSITORY.test(spec.split('#')[0])) github = spec;
  if (github !== undefined) {
    const [repository, reference, ...extra] = github.split('#');
    const normalized = repository.replace(/\.git$/i, '').toLowerCase();
    if (!REPOSITORY.test(normalized) || normalized.split('/').some(part => ['.', '..'].includes(part) || part.startsWith('-') || part.length > 100)
      || extra.length || reference === '' || /\s/.test(github)
      || reference && (reference.length > 128 || reference.startsWith('-') || reference.startsWith('/') || reference.endsWith('/') || reference.includes('..') || /[\\?:~^*\[@]/.test(reference))) throw new Error('GitHub 仓库或版本引用无效');
    return { ecosystem: 'dsh', github: { repository: normalized, ...(reference ? { reference } : {}) } };
  }
  const separator = spec.lastIndexOf('@');
  const name = separator > 0 ? spec.slice(0, separator) : spec;
  const version = separator > 0 ? spec.slice(separator + 1) : undefined;
  if (!NPM_NAME.test(name) || name.length > 214 || (version !== undefined && (!version || (!semver.validRange(version) && !/^[a-z][a-z0-9._-]*$/i.test(version))))) throw new Error('npm 插件版本或标签无效');
  return { ecosystem: 'dsh', package: name, ...(version && version !== 'latest' ? { version } : {}) };
}

export function dshBundle(plugin) {
  const { source } = plugin;
  if (source.kind !== 'upstream' || !source.location.startsWith('dsh:')) return;
  const spec = source.location.slice(4), index = spec.lastIndexOf('@');
  if (index <= 0 || !NPM_NAME.test(spec.slice(0, index))) throw new Error('daemon 上游插件身份无效');
  const name = spec.slice(0, index), version = spec.slice(index + 1);
  let dependency = version;
  if (source.github) {
    const { repository, reference, commit } = source.github;
    if (!REPOSITORY.test(repository) || !/^[0-9a-f]{40}$/.test(commit ?? '')) throw new Error('daemon GitHub 来源身份无效');
    dependency = `github:${repository}${reference ? `#${reference}` : ''}`;
  }
  return { name, version, source: dependency, plugin };
}
