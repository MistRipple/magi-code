// DSH 的插件目录读取沿 Magi daemon 的唯一安装事实投影。这里只适配公共
// 包元数据；不生成 DSH profile 文件，不读取或复制其他插件的配置。
export function createPluginManager(api, registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org') {
  return {
    async listBundles() {
      const { plugins } = await api.plugins.list();
      return plugins.flatMap(({ manifest, source, enabled, active }) => {
        if (source.kind !== 'upstream' || !source.location.startsWith('dsh:')) return [];
        const spec = source.location.slice(4), separator = spec.lastIndexOf('@');
        if (separator <= 0) throw new Error(`DSH 包来源无效：${source.location}`);
        const name = spec.slice(0, separator), version = spec.slice(separator + 1);
        return [{ name, version, enabled, active, installed: true, source: version,
          availability: 'magi', removable: true, official: name.startsWith('@deepseek-ai/'),
          readOnlyReason: '在线安装、升级和启停请使用 Magi 的插件管理入口',
          meta: { title: manifest.name, description: manifest.description } }];
      });
    },
    async registries() { return { registry }; },
  };
}
