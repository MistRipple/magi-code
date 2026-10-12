// 后台入口：每次调用启动一个新的 QuickJS 实例，状态只通过宿主 SDK 的资源读写传递。
// 同一个入口承接全部工具与命令，用 sdk.invocation.contribution 分派。

const RESOURCE = 'board';

function load(sdk) {
  const snapshot = sdk.call('resource.read', { resourceId: RESOURCE });
  const items = Array.isArray(snapshot.value?.items) ? snapshot.value.items : [];
  return { version: snapshot.version, items };
}

function save(sdk, version, items) {
  // expectedVersion 让并发修改明确冲突，而不是后到者静默覆盖。
  return sdk.call('resource.write', { resourceId: RESOURCE, expectedVersion: version, value: { items } });
}

export default function main(input, sdk) {
  const { kind, id } = sdk.invocation.contribution;
  // 数据格式转换：仅当新版清单提高 dataSchemaVersion 并声明 dataMigration.fromVersions 时，
  // 升级会逐条用旧数据调用这里。它是纯函数——此时 sdk.call 全部被拒绝，只能读 input、返回结果。
  // 返回 { value } 保留（转换后）的条目，返回 { drop: true } 丢弃它。
  if (kind === 'migration') {
    if (input.resourceId === RESOURCE) {
      const items = Array.isArray(input.value?.items) ? input.value.items : [];
      return { value: { schema: input.toVersion, items: items.map((item) => ({ ...item, migratedFrom: input.fromVersion })) } };
    }
    return { value: input.value };
  }
  if (kind === 'tool' && id === 'board-summary') {
    return load(sdk);
  }
  if (kind === 'tool' && id === 'board-add') {
    const { version, items } = load(sdk);
    const settings = sdk.call('settings.read', {});
    const limit = settings.value?.maxItems ?? 50;
    if (items.length >= limit) throw new Error('看板已满');
    const written = save(sdk, version, [...items, { text: input.text }]);
    return { version: written.version, count: items.length + 1 };
  }
  if (kind === 'command' && id === 'clear') {
    const { version } = load(sdk);
    save(sdk, version, []);
    return { content: '看板已清空。' };
  }
  throw new Error(`未处理的贡献: ${kind}/${id}`);
}
