// Schemastery（DSH 的配置 schema）序列化 → JSON Schema（Magi 的 settingsSchema / 工具 inputSchema）。
// 只翻译能精确表达的类型；任何不认识的类型或元数据都明确失败，不静默放宽。
//
// 输入是 `schema.toJSON()` 的结果：{ uid, refs: { [id]: { type, meta, inner?, dict?, list?, value? } } }。

const SUPPORTED = new Set(['string', 'number', 'boolean', 'array', 'object', 'dict', 'union', 'const']);
const KNOWN_META = new Set(['required', 'step', 'min', 'max', 'default', 'description', 'volatile', 'role', 'hidden', 'disabled', 'collapse', 'pattern', 'loose', 'comment', 'link', 'extra', 'deprecated', 'experimental']);

export class SchemaTranslationError extends Error {}

/**
 * @param {{ uid: number, refs: Record<string, any> }} serialized
 * @returns {{ schema: object, secrets: string[][], volatile: string[][] }}
 */
export function toJsonSchema(serialized) {
  const refs = serialized.refs;
  const secrets = [];
  const volatile = [];
  const seen = new Set();
  const fail = (path, message) => { throw new SchemaTranslationError(`${path.join('.') || '(根)'}：${message}`); };

  const convert = (id, path) => {
    const node = refs[id];
    if (!node) fail(path, `引用 ${id} 不存在`);
    if (seen.has(id)) fail(path, '配置 schema 存在循环引用，无法翻译为有界 JSON Schema');
    if (!SUPPORTED.has(node.type)) fail(path, `不支持的 Schemastery 类型 ${node.type}`);
    const meta = node.meta ?? {};
    for (const key of Object.keys(meta)) if (!KNOWN_META.has(key)) fail(path, `不认识的元数据 ${key}`);
    if (meta.role === 'secret') secrets.push(path);
    else if (meta.role !== undefined) fail(path, `不支持的 role：${meta.role}`);
    if (meta.volatile) volatile.push(path);
    seen.add(id);
    let out;
    switch (node.type) {
      case 'string':
        out = { type: 'string' };
        if (meta.pattern) out.pattern = typeof meta.pattern === 'string' ? meta.pattern : meta.pattern.source;
        if (meta.min !== undefined) out.minLength = meta.min;
        if (meta.max !== undefined) out.maxLength = meta.max;
        break;
      case 'number':
        out = { type: 'number' };
        if (meta.min !== undefined) out.minimum = meta.min;
        if (meta.max !== undefined) out.maximum = meta.max;
        if (meta.step !== undefined && Number.isInteger(meta.step) && meta.step === 1) out.type = 'integer';
        break;
      case 'boolean':
        out = { type: 'boolean' };
        break;
      case 'const':
        out = { const: node.value };
        break;
      case 'array':
        out = { type: 'array', items: convert(node.inner, [...path, '[]']) };
        if (meta.min !== undefined) out.minItems = meta.min;
        if (meta.max !== undefined) out.maxItems = meta.max;
        break;
      case 'dict':
        out = { type: 'object', additionalProperties: convert(node.inner, [...path, '{}']) };
        break;
      case 'object': {
        const properties = {};
        const required = [];
        for (const [key, child] of Object.entries(node.dict ?? {})) {
          properties[key] = convert(child, [...path, key]);
          if (refs[child]?.meta?.required) required.push(key);
        }
        out = { type: 'object', properties, additionalProperties: false };
        if (required.length) out.required = required;
        break;
      }
      case 'union': {
        const variants = (node.list ?? []).map((child, index) => convert(child, [...path, `|${index}`]));
        const consts = variants.every((variant) => 'const' in variant);
        out = consts ? { enum: variants.map((variant) => variant.const) } : { anyOf: variants };
        break;
      }
      default:
        fail(path, `未处理的类型 ${node.type}`);
    }
    seen.delete(id);
    if (meta.description) out.description = String(meta.description);
    if (meta.default !== undefined) out.default = meta.default;
    return out;
  };

  const schema = convert(serialized.uid, []);
  if (schema.type !== 'object') fail([], '配置 schema 的根必须是 object');
  return { schema, secrets, volatile };
}
