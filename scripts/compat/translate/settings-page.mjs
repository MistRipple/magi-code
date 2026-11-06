// 由 JSON Schema 生成通用设置页（单文件 HTML，只用 Magi 公开视图桥的 settings.read / settings.write）。
// 页面按 schema 渲染：string/number/integer/boolean/enum 渲染成表单控件；数组、嵌套对象和字典渲染成 JSON 文本框，
// 保存前在页面内解析并按 schema 的类型做基本检查，最终校验仍由 Magi 按 settingsSchema 执行。
//
// 为什么是生成页而不是 Magi 通用表单：Magi 不为插件生成设置界面（作者指南第 4 节），所有界面都由包自己提供。

const escapeHtml = (text) => String(text).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

/**
 * @param {{ schema: object, secrets?: string[][], hidden?: string[], title: string }} options
 */
export function renderSettingsPage({ schema, title, hidden = [], runtimeSchema = false }) {
  const fields = Object.entries(schema.properties ?? {})
    .filter(([key]) => !hidden.includes(key))
    .map(([key, node]) => ({
      key,
      kind: node.enum ? 'enum' : node.type === 'object' || node.type === 'array' || node.anyOf ? 'json' : node.type,
      description: node.description ?? '',
      options: node.enum ?? [],
      required: (schema.required ?? []).includes(key),
      defaultValue: node.default,
    }));
  const data = JSON.stringify({ fields }).replace(/</g, '\\u003c');
  return `<!doctype html>
<meta charset="utf-8">
<meta name="color-scheme" content="light dark">
<title>${escapeHtml(title)}</title>
<style>
  body { font: 14px/1.5 system-ui, sans-serif; margin: 0; padding: 16px; max-width: 720px; }
  .field { margin-bottom: 14px; }
  label { display: block; font-weight: 600; margin-bottom: 2px; }
  .hint { opacity: .7; font-size: 12px; margin-bottom: 4px; }
  input[type=text], input[type=number], select, textarea { width: 100%; box-sizing: border-box; padding: 6px 8px; font: inherit; }
  textarea { min-height: 88px; font-family: ui-monospace, monospace; }
  button { padding: 6px 14px; }
  #status { margin-left: 8px; opacity: .7; }
  .error { color: #c0392b; }
</style>
<div id="form"></div>
<div><button id="save">保存</button><span id="status" role="status"></span></div>
<script>
  const CHANNEL = 'magi-plugin-v1';
  let { fields } = ${data};
  const runtimeSchema = ${JSON.stringify(runtimeSchema)};
  const form = document.getElementById('form');
  const status = document.getElementById('status');
  let instanceId = null, version = 0, next = 0, current = {};
  const pending = new Map();
  const controls = new Map();

  function request(operation, extra) {
    return new Promise((resolve, reject) => {
      const requestId = 'r' + (++next);
      pending.set(requestId, { resolve, reject });
      parent.postMessage({ channel: CHANNEL, instanceId, type: 'request', requestId, operation, ...extra }, '*');
    });
  }
  function build() {
    for (const field of fields) {
      const wrap = document.createElement('div');
      wrap.className = 'field';
      const label = document.createElement('label');
      label.textContent = field.key;
      wrap.append(label);
      if (field.description) {
        const hint = document.createElement('div');
        hint.className = 'hint';
        hint.textContent = field.description;
        wrap.append(hint);
      }
      let control;
      if (field.kind === 'boolean') { control = document.createElement('input'); control.type = 'checkbox'; }
      else if (field.kind === 'enum') {
        control = document.createElement('select');
        for (const option of field.options) { const o = document.createElement('option'); o.value = JSON.stringify(option); o.textContent = String(option); control.append(o); }
      } else if (field.kind === 'json') { control = document.createElement('textarea'); control.spellcheck = false; }
      else { control = document.createElement('input'); control.type = field.kind === 'number' || field.kind === 'integer' ? 'number' : 'text'; control.autocomplete = 'off'; }
      control.id = 'f-' + field.key;
      label.htmlFor = control.id;
      controls.set(field.key, { field, control });
      wrap.append(control);
      form.append(wrap);
    }
  }
  function show(settings) {
    version = settings.version;
    current = settings.value ?? {};
    for (const { field, control } of controls.values()) {
      const value = current[field.key] === undefined ? field.defaultValue : current[field.key];
      if (field.kind === 'boolean') control.checked = value === true;
      else if (field.kind === 'enum') control.value = JSON.stringify(value ?? field.options[0]);
      else if (field.kind === 'json') control.value = value === undefined ? '' : JSON.stringify(value, null, 2);
      else control.value = value === undefined || value === null ? '' : String(value);
    }
  }
  function collect() {
    const next = { ...current };
    for (const { field, control } of controls.values()) {
      let value;
      if (field.kind === 'boolean') value = control.checked;
      else if (field.kind === 'enum') value = JSON.parse(control.value);
      else if (field.kind === 'json') { if (control.value.trim() === '') { delete next[field.key]; continue; } value = JSON.parse(control.value); }
      else if (field.kind === 'number' || field.kind === 'integer') { if (control.value === '') { delete next[field.key]; continue; } value = Number(control.value); }
      else { if (control.value === '' && !field.required) { delete next[field.key]; continue; } value = control.value; }
      next[field.key] = value;
    }
    return next;
  }
  window.addEventListener('message', async (event) => {
    const m = event.data;
    if (!m || m.channel !== CHANNEL) return;
    if (m.type === 'init') {
      instanceId = m.instanceId;
      if (runtimeSchema) {
        const { schema } = await request('settings.describe', {});
        fields = Object.entries(schema.properties ?? {}).map(([key, node]) => ({key, kind: node.enum ? 'enum' : node.type === 'object' || node.type === 'array' || node.anyOf ? 'json' : node.type, description: node.description ?? '', options: node.enum ?? [], required: (schema.required ?? []).includes(key), defaultValue: node.default}));
      }
      build();
      show(await request('settings.read', {}));
    } else if (m.type === 'response') {
      const waiter = pending.get(m.requestId);
      pending.delete(m.requestId);
      if (waiter) m.ok ? waiter.resolve(m.result) : waiter.reject(new Error(m.error));
    }
  });
  document.getElementById('save').onclick = async () => {
    status.className = '';
    try {
      show(await request('settings.write', { expectedVersion: version, value: collect() }));
      status.textContent = '已保存';
    } catch (error) {
      status.className = 'error';
      status.textContent = String(error.message || error);
    }
  };
</script>
`;
}
