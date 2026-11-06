// 规则表的共同形状与状态判定。生态目录（dsh/、pi/）只写规则，状态由这里统一算出。
import { requireCapability } from './capabilities.mjs';

/**
 * 能力映射规则。
 * @param {'imports'|'calls'|'injects'|'events'|'props'} kind
 * @param {RegExp} match
 * @param {object} spec
 *   capability  映射到的 Magi 能力 ID（见 capabilities.mjs）
 *   fidelity    'full' | 'partial' | 'none'（与 capability 一起使用）
 *   fixed       不经过能力判定的固定状态：'shim'（兼容层自己实现）| 'bundled'（纯 JS，构建期打包）| 'unknown'
 *   group       报告里的分组（缺省取能力的 group）
 *   note        原因或约束
 *   source      依据的上游文档（URL 与读取范围）
 *   needs       要把 partial/none 补成 full，Magi 侧需要的扩展（用于汇总真实缺口，不是承诺）
 */
export function rule(kind, match, spec) {
  if (spec.capability) {
    requireCapability(spec.capability);
    if (!['full', 'partial', 'none'].includes(spec.fidelity)) throw new Error(`规则 ${match} 缺少合法的 fidelity`);
  } else if (!['shim', 'bundled', 'unknown'].includes(spec.fixed)) {
    throw new Error(`规则 ${match} 必须给出 capability 或 fixed`);
  }
  return { kind, match, ...spec };
}

/**
 * 状态判定：
 *   fixed            → 原样
 *   fidelity none    → dropped
 *   fidelity partial → partial
 *   fidelity full    → 翻译器已实现该能力 ? mapped : planned
 * “Magi 能不能承载”（fidelity）与“翻译器做没做”（implemented）是两件事，分开记录。
 */
export function resolveRule(r, implemented) {
  if (r.fixed) {
    return { status: r.fixed, category: r.group ?? 'other', target: r.target ?? '', note: r.note ?? '', source: r.source, needs: r.needs };
  }
  const capability = requireCapability(r.capability);
  const status = r.fidelity === 'none' ? 'dropped' : r.fidelity === 'partial' ? 'partial' : (implemented.has(r.capability) ? 'mapped' : 'planned');
  return {
    status, category: r.group ?? capability.group,
    target: r.fidelity === 'none' ? '无' : capability.magi,
    note: r.note ?? '', source: r.source, needs: r.needs, capability: r.capability,
  };
}

export function classifyWith(rules, implemented, kind, name) {
  for (const candidate of rules) {
    if (candidate.kind === kind && candidate.match.test(name)) return resolveRule(candidate, implemented);
  }
  return { status: 'unknown', category: 'other', target: '待评审', note: '规则表没有登记', source: undefined, needs: undefined };
}
