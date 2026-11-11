// 原生菜单与上游请求共用同一映射；关闭推理的别名必须还原为该模型声明的 ID。
const NATIVE_EFFORTS = new Set(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']);
const ALIASES = new Map([['off', 'none'], ['disabled', 'none'], ['light', 'low'], ['balanced', 'medium'], ['deep', 'high']]);
function bindings(efforts) {
  const result = new Map();
  for (const raw of efforts) {
    const id = String(raw), native = ALIASES.get(id) ?? id;
    if (!NATIVE_EFFORTS.has(native)) throw new Error(`推理档位 ${id} 无法映射到 Magi 的档位枚举`);
    if (result.has(native) && result.get(native) !== id) throw new Error(`推理档位 ${id} 与 ${result.get(native)} 在 Magi 中无法区分`);
    result.set(native, id);
  }
  return result;
}

export function reasoningOf(efforts, defaultEffort) {
  const mapping = bindings(efforts);
  const declared = { supportedEfforts: [...mapping.keys()], transport: 'openai', source: 'dsh' };
  if (defaultEffort !== undefined) {
    const selected = [...mapping].find(([, id]) => id === String(defaultEffort));
    if (!selected) throw new Error(`默认推理档位 ${defaultEffort} 不在模型声明中`);
    declared.defaultEffort = selected[0];
  }
  return declared;
}

export function upstreamReasoningEffort(efforts, native) {
  const id = bindings(efforts).get(native);
  if (!id) throw new Error(`模型未声明推理档位 ${native}`);
  return id;
}

// 公共模型服务返回 Magi 的 ReasoningCapability；DSH 的 efforts/name/off
// 只在此适配，不改变回传给 daemon 校验的原始 selection 快照。
export function dshReasoningOf(capability) {
  const ids = capability.supportedEfforts;
  if (!Array.isArray(ids) || !ids.length || ids.some(id => !NATIVE_EFFORTS.has(id))) throw new Error('原生模型推理能力无效');
  const idOf = id => id === 'none' ? 'off' : id;
  const reasoning = { efforts: ids.map(id => ({ id: idOf(id), name: idOf(id) })) };
  if (capability.defaultEffort != null) {
    if (!ids.includes(capability.defaultEffort)) throw new Error('原生模型默认推理档位不在声明中');
    reasoning.defaultEffort = idOf(capability.defaultEffort);
  }
  return reasoning;
}
