// 配置变体：DSH 插件常按开关决定是否注册某些工具。清单必须静态列出工具，因此记录阶段要覆盖这些开关。
// 没有作者提供变体的通用路径：默认配置一份，加上“配置 schema 里所有布尔开关置为 true”一份，取并集。
// 并集意味着清单可能声明了某些互斥开关下才会出现的工具；运行期没有被上游注册的工具不会绑定处理器，
// 调用会明确失败，报告里会标出 variants，供用户知情。
//
// 输入是 Schemastery 的 `toJSON()`：{ uid, refs: { [id]: { type, meta, dict? } } }。

/** @returns {object|null} 没有任何布尔开关时返回 null */
export function allSwitchesOn(serialized) {
  if (!serialized?.refs || serialized.uid === undefined) return null;
  const refs = serialized.refs;
  const walk = (id, seen) => {
    const node = refs[id];
    if (!node || seen.has(id)) return undefined;
    if (node.type === 'boolean') return true;
    if (node.type !== 'object' || !node.dict) return undefined;
    const next = new Set(seen).add(id);
    const out = {};
    for (const [key, child] of Object.entries(node.dict)) {
      const value = walk(child, next);
      if (value !== undefined) out[key] = value;
    }
    return Object.keys(out).length ? out : undefined;
  };
  return walk(serialized.uid, new Set()) ?? null;
}
