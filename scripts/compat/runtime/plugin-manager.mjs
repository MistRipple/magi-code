// DSH 插件管理沿 daemon 的安装、摘要确认和生命周期入口。新包授权需要
// 管理插件单独获得 authorize 委托；失败不能把已下载当成已运行。
import { NPM_NAME as NAME, parseBundleSpec, dshBundle } from './plugin-source.mjs';
export { parseBundleSpec } from './plugin-source.mjs';
import { profileCompatibility, updateProfileExemption, createProfileMutation, profileChangeId } from './profile.mjs';

const failure = (error, extra = {}) => ({ ...error.receipt, application: 'failed', ...extra,
  error: { code: error.code ?? 'magi-operation-failed', message: String(error.message ?? error), diagnostic: String(error.message ?? error),
    ...(error.incompatible ? { incompatible: error.incompatible } : {}) } });

export function createPluginManager(api, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org', signal) {
  // 原生 DSH 管理面板允许在 HTTP 202 后继续工作；使用显式实例级管理
  // API，由 daemon 的独立 background 委托控制，不借用已结束的请求租约。
  const mutateProfile = createProfileMutation(Boolean(api.plugins.background), signal);
  if (api.plugins.background) api = { ...api, plugins: api.plugins.background };
  const find = async (name) => {
    if (!NAME.test(name)) throw new Error('npm 包名无效');
    const { plugins } = await api.plugins.list();
    signal?.throwIfAborted();
    const found = plugins.filter(plugin => dshBundle(plugin)?.name === name);
    if (found.length !== 1) throw new Error(`未找到唯一的 DSH 插件：${name}`);
    return found[0];
  };
  const manager = {
    async listBundles() {
      const { plugins } = await api.plugins.list();
      return plugins.flatMap(plugin => {
        const { manifest, enabled, active } = plugin;
        const bundle = dshBundle(plugin);
        if (!bundle) return [];
        const { name, version, source } = bundle;
        return [{ name, version, enabled, active, installed: true, source,
          availability: 'profile', removable: true, official: !plugin.source.github && name.startsWith('@deepseek-ai/'),
          ...(!api.plugins.installBundle ? { readOnlyReason: '在线安装、升级和启停请使用 Magi 的插件管理入口' } : {}),
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
  if (api.plugins.installBundle) {
    manager.installBundle = async (spec, options = {}) => {
      try {
        if (options.enabled !== undefined && typeof options.enabled !== 'boolean') throw new Error('enabled 必须是布尔值');
        if (options.approvedBuilds?.length) throw new Error('Magi 不执行安装脚本，不接受构建授权');
        if (options.registry && options.registry.replace(/\/+$/, '') !== registry.replace(/\/+$/, '')) throw new Error('安装源与 Magi 当前 registry 不一致，请先在 Magi 中配置安装源');
        const input = { ...parseBundleSpec(spec), requestId: options.requestId ?? crypto.randomUUID(),
          ...(options.enabled === undefined ? {} : { enabled: options.enabled }) };
        return await mutateProfile(() => api.plugins.installBundle({ ...input,
          ...(profileChangeId() ? { changeId: profileChangeId() } : {}) }));
      } catch (error) { return failure(error, { stage: error.receipt?.installed ? 'reconcile' : 'install' }); }
    };
  }
  if (api.plugins.cancelInstall) manager.cancelInstall = (requestId) => api.plugins.cancelInstall(requestId);
  if (api.plugins.setEnabled) manager.setBundleEnabled = async (name, enabled) => {
    try {
      if (typeof enabled !== 'boolean') throw new Error('enabled 必须是布尔值');
      if (!NAME.test(name)) throw new Error('npm 包名无效');
      return await mutateProfile(async () => {
        const plugin = await find(name);
        await api.plugins.setEnabled(plugin.manifest.id, enabled, plugin.digest);
        return { bundle: name, application: 'applied' };
      });
    } catch (error) { return failure(error); }
  };
  if (api.plugins.remove) manager.removeBundle = async (name) => {
    try {
      if (!NAME.test(name)) throw new Error('npm 包名无效');
      return await mutateProfile(async () => {
        const plugin = await find(name);
        const changeId = profileChangeId(true);
        await api.plugins.remove(plugin.manifest.id, plugin.digest, changeId ? { changeId } : {});
        return { bundle: name, application: 'applied' };
      });
    } catch (error) { return failure(error); }
  };
  return manager;
}
