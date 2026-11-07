// 仅为没有 ./client 的包提供配置入口；原生页面不经过此组件。
// 与原生页面共用 settingsScope、视图桥、主题和生命周期，不生成第二套页面运行时。
import { createElement as h, useEffect, useRef, useState, useSyncExternalStore } from 'react';

const labels = {
  zh: { save: '保存', saving: '保存中…', saved: '已保存', failed: '保存失败，请检查配置后重试' },
  en: { save: 'Save', saving: 'Saving…', saved: 'Saved', failed: 'Could not save. Check the settings and retry.' },
};
const kindOf = node => node.enum ? 'enum' : node.type === 'object' || node.type === 'array' || node.anyOf ? 'json' : node.type;

export function SettingsForm({ scope, schema, locale, error }) {
  const snapshot = useSyncExternalStore(scope.subscribe, scope.getSnapshot, scope.getSnapshot);
  const controls = useRef(new Map());
  const [saving, setSaving] = useState(false), [status, setStatus] = useState('');
  const text = labels[locale.startsWith('zh') ? 'zh' : 'en'];
  const ready = snapshot.status === 'ready' && !!schema && !error;
  useEffect(() => {
    if (!ready) return;
    for (const [key, node] of Object.entries(schema.properties ?? {})) {
      const control = controls.current.get(key), value = snapshot.value[key], kind = kindOf(node);
      if (!control) continue;
      if (kind === 'boolean') control.checked = value === true;
      else if (kind === 'enum') control.value = JSON.stringify(value ?? node.enum[0]);
      else if (kind === 'json') control.value = value === undefined ? '' : JSON.stringify(value, null, 2);
      else control.value = value ?? '';
    }
  }, [snapshot, schema, ready]);
  const save = async event => {
    event.preventDefault();
    if (!ready || saving) return;
    setSaving(true); setStatus('');
    try {
      const operations = Object.entries(schema.properties ?? {}).map(([key, node]) => {
        const control = controls.current.get(key), kind = kindOf(node);
        if (kind !== 'boolean' && kind !== 'enum' && control.value === '' && !schema.required?.includes(key)) return { op: 'unset', path: [key] };
        const value = kind === 'boolean' ? control.checked : kind === 'enum' || kind === 'json' ? JSON.parse(control.value)
          : kind === 'number' || kind === 'integer' ? Number(control.value) : control.value;
        return { op: 'set', path: [key], value };
      });
      if (!await scope.mutate(operations, snapshot.revision)) throw new Error(text.failed);
      setStatus(text.saved);
    } catch (failure) { setStatus(String(failure.message ?? failure)); }
    finally { setSaving(false); }
  };
  const style = { inlineSize: '100%', boxSizing: 'border-box', padding: '6px 8px', font: 'inherit', color: 'inherit', background: 'var(--surface-1)', border: '0.5px solid var(--border)', borderRadius: 'var(--radius-sm)' };
  return h('div', { style: { display: 'grid', gap: 'var(--space-3, 12px)', minInlineSize: 0 } },
    ...Object.entries(schema?.properties ?? {}).map(([key, node]) => {
      const kind = kindOf(node), props = { id: `f-${key}`, ref: element => { if (element) controls.current.set(key, element); else controls.current.delete(key); }, disabled: !ready || saving, style };
      const control = kind === 'enum' ? h('select', props, ...node.enum.map(value => h('option', { key: JSON.stringify(value), value: JSON.stringify(value) }, String(value))))
        : kind === 'json' ? h('textarea', { ...props, rows: 4, spellCheck: false })
        : h('input', { ...props, style: kind === 'boolean' ? undefined : style, type: kind === 'boolean' ? 'checkbox' : kind === 'number' || kind === 'integer' ? 'number' : 'text', step: kind === 'integer' ? 1 : 'any', autoComplete: 'off' });
      return h('div', { key, style: { minInlineSize: 0 } }, h('label', { htmlFor: props.id }, key),
        node.description && h('div', { style: { color: 'var(--foreground-muted)', overflowWrap: 'anywhere' } }, node.description), control);
    }),
    h('div', null, h('button', { id: 'save', type: 'button', onClick: save, disabled: !ready || saving, style: { padding: '8px 16px', minBlockSize: 40, font: 'inherit', color: 'var(--btn-primary-fg)', background: 'var(--btn-primary-bg)', border: 0, borderRadius: 'var(--radius-sm)' } }, saving ? text.saving : text.save),
      h('span', { id: 'status', role: 'status', style: { marginInlineStart: 8, overflowWrap: 'anywhere' } }, error || status)));
}
