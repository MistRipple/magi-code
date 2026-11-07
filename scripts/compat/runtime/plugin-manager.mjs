// DSH 插件管理沿 daemon 的安装、摘要确认和生命周期入口。新包授权需要
// 管理插件单独获得 authorize 委托；失败不能把已下载当成已运行。
import semver from 'semver';
import pacote from 'pacote';
import { profileCompatibility, updateProfileExemption } from './profile.mjs';
import { checkDshCompatibility } from '../core/dsh-compatibility.mjs';

const NAME = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/;
const failure = (error, extra = {}) => ({ application: 'failed', ...extra,
  error: { code: error.code ?? 'magi-operation-failed', message: String(error.message ?? error), diagnostic: String(error.message ?? error),
    ...(error.incompatible ? { incompatible: error.incompatible } : {}) } });

export function parseBundleSpec(spec) {
  if (typeof spec !== 'string' || spec.length > 300) throw new Error('npm 插件来源无效');
  const separator = spec.lastIndexOf('@');
  const name = separator > 0 ? spec.slice(0, separator) : spec;
  const version = separator > 0 ? spec.slice(separator + 1) : undefined;
  if (!NAME.test(name) || (version !== undefined && (!version.trim() || (!semver.validRange(version) && !/^[a-z][a-z0-9._-]*$/i.test(version))))) throw new Error('npm 插件版本或标签无效；不执行 Git、路径或安装脚本');
  return { ecosystem: 'dsh', package: name, ...(version && version !== 'latest' ? { version } : {}) };
}

export function createPluginManager(api, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org') {
  // 原生 DSH 管理面板允许在 HTTP 202 后继续工作；使用显式实例级管理
  // API，由 daemon 的独立 background 委托控制，不借用已结束的请求租约。
  if (api.plugins.background) api = { ...api, plugins: api.plugins.background };
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
          availability: 'profile', removable: true, official: name.startsWith('@deepseek-ai/'),
          ...(!api.plugins.install ? { readOnlyReason: '在线安装、升级和启停请使用 Magi 的插件管理入口' } : {}),
          meta: { title: manifest.name, description: manifest.description } }];
      });
    },
    async registries() { return { registry }; },
    listVersionExemptions() {
      const { runtimeVersion, exemptions } = profileCompatibility();
      return { runtimeVersion, exemptions };
    },
    async setVersionExemption(target, runtimeVersion, enabled, acceptRisk = false) {
      const { exemptions } = await updateProfileExemption(target, runtimeVersion, enabled, acceptRisk);
      return { target, runtimeVersion, enabled, exemptions, application: 'applied' };
    },
  };
  if (api.plugins.preview && api.plugins.install) {
    manager.installBundle = async (spec, options = {}) => {
      let installed, preview;
      try {
        if (options.enabled !== undefined && typeof options.enabled !== 'boolean') throw new Error('enabled 必须是布尔值');
        if (options.approvedBuilds?.length) throw new Error('Magi 不执行安装脚本，不接受构建授权');
        if (options.registry && options.registry.replace(/\/+$/, '') !== registry.replace(/\/+$/, '')) throw new Error('安装源与 Magi 当前 registry 不一致，请先在 Magi 中配置安装源');
        const request = parseBundleSpec(spec);
        if ((request.version && !semver.valid(request.version)) || api.plugins.compatibility) {
          const manifest = await pacote.manifest(`${request.package}@${request.version ?? 'latest'}`, { registry, ignoreScripts: true });
          if (manifest.name !== request.package || !semver.valid(manifest.version)) throw new Error('registry 返回的插件身份无效');
          request.version = manifest.version;
          // 与翻译器共用上游版本规则，为原生页面保留可处理的精确诊断。
          // 此处只预检；真正安装仍由 daemon 在翻译和提交时重新准入。
          if (api.plugins.compatibility) {
            const policy = await api.plugins.compatibility();
            checkDshCompatibility(manifest, policy, policy);
          }
        }
        preview = await api.plugins.preview(request);
        const list = await api.plugins.install({ ...request, version: preview.version, expectedDigest: preview.candidate.digest });
        installed = list.plugins.find(({ source }) => source.location === `dsh:${request.package}@${preview.version}`);
        if (!installed) throw new Error('安装结果缺少插件记录');
        if (options.enabled === false && (installed.enabled || installed.active)) {
          await api.plugins.setEnabled(installed.manifest.id, false, installed.digest);
        }
        if (options.enabled !== false) {
          if (api.plugins.authorize && !installed.active) {
            await api.plugins.authorize(installed.manifest.id, installed.digest, installed.manifest.permissions);
          }
          const updated = await api.plugins.setEnabled(installed.manifest.id, true, installed.digest);
          installed = updated.plugins.find((plugin) => plugin.manifest.id === installed.manifest.id);
          if (!installed?.active) throw new Error('插件尚未激活');
        }
        return { bundle: request.package, application: 'applied', version: preview.version,
          changed: ['install', 'upgrade'].includes(preview.candidate.operation) };
      } catch (error) {
        if (installed) {
          const authorization = error.kind === 'PermissionDenied';
          return failure(Object.assign(new Error(`已安装 ${installed.manifest.name}，后续操作未完成：${error.message}${authorization ? '；请在 Magi 插件管理中确认权限后激活' : ''}`), { code: authorization ? 'authorization-required' : 'activation-failed' }), {
            bundle: parseBundleSpec(spec).package, installed: true, stage: 'enable', version: preview.version,
            changed: ['install', 'upgrade'].includes(preview.candidate.operation),
          });
        }
        return failure(error, { stage: 'install' });
      }
    };
  }
  if (api.plugins.setEnabled) manager.setBundleEnabled = async (name, enabled) => {
    try {
      if (typeof enabled !== 'boolean') throw new Error('enabled 必须是布尔值');
      const plugin = await find(name);
      await api.plugins.setEnabled(plugin.manifest.id, enabled, plugin.digest);
      return { bundle: name, application: 'applied' };
    } catch (error) { return failure(error); }
  };
  if (api.plugins.remove) manager.removeBundle = async (name) => {
    try {
      const plugin = await find(name);
      await api.plugins.remove(plugin.manifest.id, plugin.digest);
      return { bundle: name, application: 'applied' };
    } catch (error) { return failure(error); }
  };
  return manager;
}
