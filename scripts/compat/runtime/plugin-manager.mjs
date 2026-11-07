// DSH 插件管理沿 daemon 的安装、摘要确认和生命周期入口。安装不替用户
// 授权新包；需要授权时返回明确的失败状态，不能把包已下载当成已运行。
import semver from 'semver';
import pacote from 'pacote';

const NAME = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/;
const failure = (error, extra = {}) => ({ application: 'failed', ...extra,
  error: { code: error.code ?? 'magi-operation-failed', message: String(error.message ?? error) } });

export function parseBundleSpec(spec) {
  if (typeof spec !== 'string' || spec.length > 300) throw new Error('npm 插件来源无效');
  const separator = spec.lastIndexOf('@');
  const name = separator > 0 ? spec.slice(0, separator) : spec;
  const version = separator > 0 ? spec.slice(separator + 1) : undefined;
  if (!NAME.test(name) || (version !== undefined && (!version.trim() || (!semver.validRange(version) && !/^[a-z][a-z0-9._-]*$/i.test(version))))) throw new Error('npm 插件版本或标签无效；不执行 Git、路径或安装脚本');
  return { ecosystem: 'dsh', package: name, ...(version && version !== 'latest' ? { version } : {}) };
}

export function createPluginManager(api, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org') {
  const find = async (name) => {
    if (!NAME.test(name)) throw new Error('npm 包名无效');
    const { plugins } = await api.plugins.list();
    const found = plugins.filter(({ source }) => source.kind === 'upstream' && source.location.startsWith(`dsh:${name}@`));
    if (found.length !== 1) throw new Error(`未找到唯一的 DSH 插件：${name}`);
    return found[0];
  };
  const manager = {
    async listBundles() {
      const { plugins } = await api.plugins.list();
      return plugins.flatMap(({ manifest, source, enabled, active }) => {
        if (source.kind !== 'upstream' || !source.location.startsWith('dsh:')) return [];
        const spec = source.location.slice(4), separator = spec.lastIndexOf('@');
        if (separator <= 0) throw new Error(`DSH 包来源无效：${source.location}`);
        const name = spec.slice(0, separator), version = spec.slice(separator + 1);
        return [{ name, version, enabled, active, installed: true, source: version,
          availability: 'magi', removable: true, official: name.startsWith('@deepseek-ai/'),
          ...(!api.plugins.install ? { readOnlyReason: '在线安装、升级和启停请使用 Magi 的插件管理入口' } : {}),
          meta: { title: manifest.name, description: manifest.description } }];
      });
    },
    async registries() { return { registry }; },
  };
  if (api.plugins.preview && api.plugins.install) {
    manager.installBundle = async (spec, options = {}) => {
      let installed;
      try {
        if (options.enabled !== undefined && typeof options.enabled !== 'boolean') throw new Error('enabled 必须是布尔值');
        if (options.approvedBuilds?.length) throw new Error('Magi 不执行安装脚本，不接受构建授权');
        if (options.registry && options.registry.replace(/\/+$/, '') !== registry.replace(/\/+$/, '')) throw new Error('安装源与 Magi 当前 registry 不一致，请先在 Magi 中配置安装源');
        const request = parseBundleSpec(spec);
        if (request.version && !semver.valid(request.version)) {
          const manifest = await pacote.manifest(`${request.package}@${request.version}`, { registry, ignoreScripts: true });
          if (manifest.name !== request.package || !semver.valid(manifest.version)) throw new Error('registry 返回的插件身份无效');
          request.version = manifest.version;
        }
        const preview = await api.plugins.preview(request);
        const list = await api.plugins.install({ ...request, version: preview.version, expectedDigest: preview.candidate.digest });
        installed = list.plugins.find(({ source }) => source.location === `dsh:${request.package}@${preview.version}`);
        if (!installed) throw new Error('安装结果缺少插件记录');
        if (options.enabled === false && (installed.enabled || installed.active)) {
          await api.plugins.setEnabled(installed.manifest.id, false);
        }
        if (options.enabled !== false) {
          const updated = await api.plugins.setEnabled(installed.manifest.id, true);
          installed = updated.plugins.find((plugin) => plugin.manifest.id === installed.manifest.id);
          if (!installed?.active) throw new Error('插件尚未激活');
        }
        return { bundle: request.package, application: 'applied', version: preview.version };
      } catch (error) {
        if (installed) {
          const authorization = error.kind === 'PermissionDenied';
          return failure(Object.assign(new Error(`已安装 ${installed.manifest.name}，后续操作未完成：${error.message}${authorization ? '；请在 Magi 插件管理中确认权限后激活' : ''}`), { code: authorization ? 'authorization-required' : 'activation-failed' }), { bundle: parseBundleSpec(spec).package, installed: true });
        }
        return failure(error);
      }
    };
  }
  if (api.plugins.setEnabled) manager.setBundleEnabled = async (name, enabled) => {
    try {
      if (typeof enabled !== 'boolean') throw new Error('enabled 必须是布尔值');
      const plugin = await find(name);
      await api.plugins.setEnabled(plugin.manifest.id, enabled);
      return { bundle: name, application: 'applied' };
    } catch (error) { return failure(error); }
  };
  if (api.plugins.remove) manager.removeBundle = async (name) => {
    try {
      const plugin = await find(name);
      await api.plugins.remove(plugin.manifest.id);
      return { bundle: name, application: 'applied' };
    } catch (error) { return failure(error); }
  };
  return manager;
}
