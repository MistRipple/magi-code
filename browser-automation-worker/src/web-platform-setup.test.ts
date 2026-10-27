import assert from "node:assert/strict";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { PLATFORM_SETUP_SCRIPT, sanitizePlatformSetupResult } from "./web-platform-setup.js";

test("平台配置只返回固定状态与合法 Tunnel id，任意页面正文和密钥不会透传", () => {
  assert.deepEqual(sanitizePlatformSetupResult({ status: "ready", tunnel_id: "tunnel_abc-123", api_key: "sk-secret", text: "private page" }), {
    status: "ready", tunnel_id: "tunnel_abc-123",
  });
  assert.deepEqual(sanitizePlatformSetupResult({ status: "login_required", tunnel_id: "tunnel_hidden", api_key: "sk-secret" }), { status: "login_required" });
  for (const tunnel_id of ["", "sk-secret", "tunnel_a b", "tunnel_" + "x".repeat(161)]) {
    assert.equal(sanitizePlatformSetupResult({ status: "ready", tunnel_id }), null);
  }
  for (const value of [null, 1, {}, { status: "SECRET_PAGE_TEXT" }]) assert.equal(sanitizePlatformSetupResult(value), null);
});

type PageNode = {
  control?: PageNode;
  textContent: string;
  getClientRects: () => number[];
  getBoundingClientRect: () => { width: number; height: number };
  querySelectorAll: (selector: string) => PageNode[];
  getAttribute: (name: string) => string | null;
};
function node(textContent: string, children: Record<string, PageNode[]> = {}): PageNode {
  return { textContent, getClientRects: () => [1], getBoundingClientRect: () => ({ width: 10, height: 10 }), getAttribute: () => null, querySelectorAll: selector =>
    [...new Set(selector.split(',').flatMap(part => children[part.trim()] ?? []))] };
}
async function inspect(children: Record<string, PageNode[]>) {
  let writes = 0;
  const result = await runInNewContext(`${PLATFORM_SETUP_SCRIPT}('inspect_tunnel')`, {
    location: { origin: 'https://platform.openai.com', pathname: '/settings/organization/tunnels' },
    document: { ...node('', children), readyState: 'complete' },
    __magiSetupInput: { perform: () => { writes++; throw new Error('inspection must not write'); } },
    getComputedStyle: (el: PageNode & { testStyle?: object }) => ({ display: "block", visibility: "visible", opacity: "1", ...el.testStyle }),
    setTimeout: (callback: () => void) => callback(),
  });
  assert.equal(writes, 0);
  return sanitizePlatformSetupResult(result);
}

test('仅有创建按钮或尚在加载的空表格不能作为 Tunnel 缺少的证据', async () => {
  const button = [node('Create tunnel')];
  assert.equal((await inspect({ button, table: [node('')] }))?.status, 'permission_or_page_required');
  assert.equal((await inspect({ button, p: [node('No tunnels yet')], '[aria-busy="true"]': [node('')] }))?.status, 'permission_or_page_required');
  assert.equal((await inspect({ button, p: [node('No tunnels yet')] }))?.status, 'missing');
});

test('未关闭表单不触发重复创建，已存在 Tunnel 从独立单元格读取完整 id', async () => {
  const button = [node('Create tunnel')];
  assert.equal((await inspect({ button, p: [node('No tunnels yet')], dialog: [node('Create')] }))?.status, 'manual_required');
  const row = node('Magitunnel_123', { td: [node('Magi'), node('tunnel_123')] });
  assert.deepEqual(await inspect({ button, tr: [row] }), { status: 'ready', tunnel_id: 'tunnel_123' });
  assert.equal((await inspect({ button, tr: [row, node('Magi', { td: [node('Magi'), node('tunnel_456')] })] }))?.status, 'ambiguous_tunnel');
  const idOnly = node('tunnel_123', { td: [node('tunnel_123'), node('')] });
  assert.equal((await inspect({ button, tr: [idOnly] }))?.status, 'tunnel_id_required');
});

test('创建 Tunnel 填写必需描述，工作空间尚未指定时暂停，不能提交默认组织范围', async () => {
  const create = node('Create tunnel');
  const name = node('');
  const description = node('');
  const workspace = node('Search workspaces by name or ID');
  const save = node('Create');
  const form = node('', { 'input[type="text"]': [name], textarea: [description], button: [workspace, save] });
  let opened = false;
  const actions: Array<{ kind: string; element: PageNode; value: string | undefined }> = [];
  const document = {
    ...node(''), readyState: 'complete',
    querySelectorAll: (selector: string) => selector.split(',').flatMap(part => {
      if (part.trim() === 'button') return [create];
      if (part.trim() === 'p') return [node('No tunnels yet')];
      if (part.trim() === 'dialog' && opened) return [form];
      return [];
    }),
  };
  const result = await runInNewContext(`${PLATFORM_SETUP_SCRIPT}('create_tunnel')`, {
    location: { origin: 'https://platform.openai.com', pathname: '/settings/organization/tunnels' }, document,
    __magiSetupInput: { perform: async (kind: string, element: PageNode, value?: string) => {
      actions.push({ kind, element, value });
      if (element === create) opened = true;
    } },
    getComputedStyle: (el: PageNode & { testStyle?: object }) => ({ display: "block", visibility: "visible", opacity: "1", ...el.testStyle }),
    setTimeout: (callback: () => void) => callback(),
  });
  assert.deepEqual(sanitizePlatformSetupResult(result), { status: 'workspace_required' });
  assert.equal(actions.find(action => action.element === name)?.value, 'Magi');
  assert.ok(actions.find(action => action.element === description)?.value);
  assert.equal(actions.some(action => action.element === save), false);
  assert.equal(actions.filter(action => action.element === workspace).length, 2);
});

// ID-only rows use icon buttons. Inspect details through native input, then Cancel.
async function inspectIdOnly(names: string[], options: { changed?: boolean; takeover?: boolean; loadingName?: boolean } = {}) {
  let dialog: PageNode | null = null;
  let activeId = '';
  let changed = false;
  let nameReads = 0;
  const actions: string[] = [];
  const rows = names.map((name, index) => {
    const id = `tunnel_${index}`;
    const edit = { ...node(''), getAttribute: (key: string) => key === 'aria-label' ? 'Edit tunnel' : null };
    return { id, name, edit, row: node(id, { td: [node(id), node('')], button: [edit] }) };
  });
  const result = await runInNewContext(`${PLATFORM_SETUP_SCRIPT}('inspect_tunnel')`, {
    location: { origin: 'https://platform.openai.com', pathname: '/settings/organization/tunnels' },
    document: {
      ...node(''), readyState: 'complete',
      querySelectorAll: (selector: string) => [...new Set(selector.split(',').flatMap(part => {
        if (part.trim() === 'tr') return changed ? [] : rows.map(item => item.row);
        if (part.trim() === 'dialog') return dialog ? [dialog] : [];
        return [];
      }))],
    },
    __magiSetupInput: { perform: async (kind: string, element: PageNode) => {
      assert.equal(kind, 'click', 'inspection must not type or save fields');
      const entry = rows.find(row => row.edit === element);
      if (entry) {
        if (options.takeover) throw new Error('user takeover');
        activeId = entry.id;
        actions.push(`edit:${activeId}`);
        const nameInput = { ...node(''), get value() { return options.loadingName && nameReads++ < 2 ? '' : entry.name; } };
        dialog = node('', { h2: [node('Edit tunnel')], 'input[type="text"]': [nameInput], button: [node('Cancel')] });
      } else {
        assert.equal(element.textContent, 'Cancel', 'must never submit detail edits');
        actions.push(`cancel:${activeId}`);
        dialog = null;
        changed = Boolean(options.changed);
      }
    } },
    getComputedStyle: (el: PageNode & { testStyle?: object }) => ({ display: "block", visibility: "visible", opacity: "1", ...el.testStyle }),
    setTimeout: (callback: () => void) => callback(),
  });
  return { result: sanitizePlatformSetupResult(result), actions };
}

test('ID-only 列表逐行回读详情名称，只绑定唯一 Magi，取消详情且不写入', async () => {
  const { result, actions } = await inspectIdOnly(['Other', 'Magi'], { loadingName: true });
  assert.deepEqual(result, { status: 'ready', tunnel_id: 'tunnel_1' });
  assert.deepEqual(actions, ['edit:tunnel_0', 'cancel:tunnel_0', 'edit:tunnel_1', 'cancel:tunnel_1']);
});

test('详情同名、名称缺失、列表变化都不能自动猜测绑定或重复创建', async () => {
  assert.equal((await inspectIdOnly(['Magi', 'Magi'])).result?.status, 'ambiguous_tunnel');
  assert.equal((await inspectIdOnly(['Other'])).result?.status, 'tunnel_id_required');
  assert.equal((await inspectIdOnly([''])).result?.status, 'tunnel_id_required');
  assert.equal((await inspectIdOnly(['Magi'], { changed: true })).result?.status, 'tunnel_id_required');
  await assert.rejects(inspectIdOnly(['Magi'], { takeover: true }), /user takeover/);
});

async function prepareKey(options: { projects?: string[]; extraPermissions?: boolean; takeover?: boolean; heading?: string; hiddenControl?: boolean; labelledExpiry?: boolean; labelledProject?: boolean; labelledPermission?: boolean; semanticPermission?: boolean; loadingProject?: boolean; delayedExpiry?: boolean; nativeExpiry?: boolean } = {}) {
  let menu = '', projectValue = 'Select project...', expiryValue = '有效期 选择有效期', restricted = false;
  const selected = new Set<string>();
  const attrNode = (text: string, attrs: Record<string, string>) => ({ ...node(text), getAttribute: (key: string) => attrs[key] ?? null });
  const name = { ...attrNode('', { placeholder: 'My Test Key' }), value: '' };
  const project = { ...node(''), getAttribute: (key: string) => options.labelledProject && key === 'aria-labelledby' ? 'project-label' : null }; Object.defineProperty(project, 'textContent', { get: () => projectValue });
  const expiry = { ...node(''), getAttribute: (key: string) => options.labelledExpiry
    ? key === 'aria-labelledby' ? 'expiry-label expiry-value' : null
    : key === 'aria-label' ? expiryValue : null };
  const nativeOptions = [{ ...node('选择有效期'), value: '' }, { ...node('1 天'), value: '1' }, { ...node('30 天'), value: '30' }];
  if (options.nativeExpiry) Object.assign(expiry, { tagName: 'SELECT', options: nativeOptions, value: '', selectedOptions: [nativeOptions[0]] });
  const radio = { ...node('Restricted'), getAttribute: (key: string) => key === 'aria-checked' ? String(restricted) : null };
  const tunnel = { ...node('Tunnels'), disabled: true };
  const permission = { ...node(''), getAttribute: (key: string) => options.labelledPermission && key === 'aria-labelledby' ? 'tunnel-label' : null }; Object.defineProperty(permission, 'textContent', { get: () => selected.size === 2 ? 'All selected' : selected.size ? [...selected][0] : 'None' });
  const generate = node('Create secret key');
  const hiddenControl = { ...node('有效期 选择有效期'), testStyle: { opacity: '0' } };
  let projectReads = 0;
  const loadingProject = { ...node('Loading projects...'), disabled: true };
  const projectTitle = { ...node('Project'), get control() { return options.loadingProject && projectReads < 4 ? loadingProject : project; } };
  const form = node('', { h2: [node(options.heading ?? 'Create new secret key')], label: options.labelledProject || options.loadingProject ? [projectTitle] : [], 'input[type="text"]': [name], '[role="radio"]': [radio], select: options.nativeExpiry ? [expiry] : [], button: [...(options.hiddenControl ? [hiddenControl] : []), project, ...(options.nativeExpiry ? [] : [expiry]), tunnel, ...(options.semanticPermission ? [] : [permission]), generate] });
  const summary = node('');
  Object.defineProperty(summary, 'textContent', { get: () => `${selected.size + (options.extraPermissions ? 1 : 0)} selected permissions` });
  const query = form.querySelectorAll;
  let expiryReads = 0;
  form.querySelectorAll = selector => {
    if (selector.includes('[aria-haspopup="listbox"]')) projectReads++;
    return selector === 'span, p, div' ? [summary] : (options.semanticPermission && selector === 'button, [role="button"], [role="combobox"]'
      ? [project, expiry, tunnel, permission, generate] : query(selector))
      .map(el => el === project && options.loadingProject && projectReads < 4 ? loadingProject : el)
      .filter(el => el !== expiry || !options.delayedExpiry || ++expiryReads > 4);
  };
  const option = (value: string) => ({ ...node(value), getAttribute: (key: string) => key === 'aria-selected' ? String(selected.has(value)) : null });
  const projects = (options.projects ?? ['Default project']).map(option);
  const expiryOptions = [option('1 天'), option('30 天')];
  const permissions = [option('Read'), option('Use')];
  const result = await runInNewContext(`${PLATFORM_SETUP_SCRIPT}('prepare_runtime_key')`, {
    location: { origin: 'https://platform.openai.com', pathname: '/settings/organization/api-keys' },
    document: {
      ...node(''), readyState: 'complete',
      getElementById: (id: string) => id === 'project-label' ? node('Project') : id === 'tunnel-label' ? node('Tunnels') : id === 'expiry-label' ? node('有效期') : id === 'expiry-value' ? node(expiryValue) : null,
      querySelectorAll: (selector: string) => [...new Set(selector.split(',').flatMap(part => {
        if (part.trim() === 'dialog') return [form];
        if (part.trim() === '[role="listbox"]') return menu ? [node('')] : [];
        if (part.trim() === '[role="option"]') return menu === 'project' ? projects : menu === 'expiry' ? expiryOptions : menu === 'permissions' ? permissions : [];
        return [];
      }))],
    },
    __magiSetupInput: { perform: async (kind: string, el: PageNode, value?: string) => {
      if (options.takeover) throw new Error('user takeover');
      assert.notEqual(el, generate, 'preparation must not generate credentials');
      assert.notEqual(el, hiddenControl, 'hidden form proxy must never receive input');
      assert.notEqual(el, loadingProject, 'loading placeholder must never receive input');
      if (el === expiry && kind === 'select') {
        Object.assign(expiry, { value, selectedOptions: nativeOptions.filter(option => option.value === value) });
        expiryValue = '有效期 30 天';
        return;
      }
      if (el === name) name.value = value ?? '';
      if (el === project) menu = menu === 'project' ? '' : 'project';
      if (el === expiry) menu = menu === 'expiry' ? '' : 'expiry';
      if (el === permission) menu = menu === 'permissions' ? '' : 'permissions';
      if (el === radio) restricted = true;
      if (projects.includes(el)) { projectValue = el.textContent; menu = ''; }
      if (expiryOptions.includes(el)) { expiryValue = '有效期 ' + el.textContent; menu = ''; }
      if (permissions.includes(el)) selected.add(el.textContent);
    } },
    getComputedStyle: (el: PageNode & { testStyle?: object }) => ({ display: "block", visibility: "visible", opacity: "1", ...el.testStyle }),
    setTimeout: (callback: () => void) => callback(),
  });
  return { result: sanitizePlatformSetupResult(result), name: name.value, project: projectValue, expiry: expiryValue, restricted, selected: [...selected] };
}

test('自动准备唯一项目、30 天和仅 Tunnels Read+Use，但不生成或回读密钥', async () => {
  const prepared = await prepareKey();
  assert.equal(prepared.result?.status, 'key_generation_required');
  assert.equal(prepared.name, 'Magi Tunnel Runtime');
  assert.equal(prepared.project, 'Default project');
  assert.equal(prepared.expiry, '有效期 30 天');
  assert.equal(prepared.restricted, true);
  assert.deepEqual(prepared.selected, ['Read', 'Use']);
});

test('项目歧义、额外权限或接管不能误报密钥表单已准备', async () => {
  assert.equal((await prepareKey({ projects: ['Project A', 'Project B'] })).result?.status, 'project_required');
  assert.equal((await prepareKey({ extraPermissions: true })).result?.status, 'key_permissions_required');
  await assert.rejects(prepareKey({ takeover: true }), /user takeover/);
});

test('真实平台标题内的多空格、换行与不换行空格不阻断表单准备', async () => {
  const prepared = await prepareKey({ heading: 'Create new\n secret\u00a0 key' });
  assert.equal(prepared.result?.status, 'key_generation_required');
  assert.deepEqual(prepared.selected, ['Read', 'Use']);
});

test('表单中有布局盒但透明的同名代理控件不能成为有效期输入目标', async () => {
  assert.equal((await prepareKey({ hiddenControl: true })).result?.status, 'key_generation_required');
});

test('有效期使用 aria-labelledby 组合名称时仍能定位并回读 30 天', async () => {
  assert.equal((await prepareKey({ labelledExpiry: true })).result?.status, 'key_generation_required');
});

test('选择项目后等待异步出现的有效期字段，再继续权限准备', async () => {
  assert.equal((await prepareKey({ delayedExpiry: true })).result?.status, 'key_generation_required');
});

test('原生有效期 select 按语义选择并回读，不把系统菜单当 DOM listbox', async () => {
  assert.equal((await prepareKey({ nativeExpiry: true })).result?.status, 'key_generation_required');
});

test('项目的可访问名称只有字段标签时，仍按可见占位文字判断是否需要选择', async () => {
  assert.equal((await prepareKey({ labelledProject: true })).result?.status, 'key_generation_required');
});

test('项目加载占位控件不能被当作已选项目，等待真实控件后继续准备', async () => {
  const prepared = await prepareKey({ loadingProject: true });
  assert.equal(prepared.result?.status, 'key_generation_required');
  assert.equal(prepared.project, 'Default project');
});

test('Tunnels 权限控件用资源名作为 accessible name 时，以可见选择值回读权限', async () => {
  assert.equal((await prepareKey({ labelledPermission: true })).result?.status, 'key_generation_required');
});

test('权限触发器为语义 button 而非原生 button 时仍选择正确资源', async () => {
  assert.equal((await prepareKey({ semanticPermission: true })).result?.status, 'key_generation_required');
});

async function continueTunnel(workspaces: string[], selected = false, persistentMenu = false) {
  let opened = true, menu = false, created = 0;
  const name = { ...node(''), value: 'Magi' };
  const description = { ...node(''), value: 'User description' };
  const workspace = node(''); let workspaceName = selected ? 'Selected workspace' : 'Search workspaces by name or ID';
  Object.defineProperty(workspace, 'textContent', { get: () => workspaceName });
  const choices = workspaces.map(value => node(value));
  const save = node('Create');
  const form = node('', { h2: [node('Create tunnel')], 'input[type="text"]': [name], textarea: [description], button: [workspace, save] });
  const row = node('Magi', { td: [node('Magi'), node('tunnel_created')] });
  const result = await runInNewContext(`${PLATFORM_SETUP_SCRIPT}('create_tunnel')`, {
    location: { origin: 'https://platform.openai.com', pathname: '/settings/organization/tunnels' },
    document: { ...node(''), readyState: 'complete', querySelectorAll: (selector: string) => [...new Set(selector.split(',').flatMap(part => {
      if (part.trim() === 'dialog') return opened ? [form] : [];
      if (part.trim() === '[role="option"]') return menu ? choices : [];
      if (part.trim() === '[role="listbox"]') return menu ? [node('')] : [];
      if (part.trim() === 'tr') return created ? [row] : [];
      return [];
    }))] },
    __magiSetupInput: { perform: async (kind: string, el: PageNode) => {
      assert.equal(kind, 'click', 'resuming a prepared form must not overwrite its fields');
      if (el === workspace) menu = !menu;
      if (choices.includes(el)) { workspaceName = el.textContent; menu = persistentMenu; }
      if (el === save) { assert.equal(menu, false, 'must close the workspace menu before submitting'); assert.notEqual(workspaceName, 'Search workspaces by name or ID'); created++; opened = false; }
    } },
    getComputedStyle: (el: PageNode & { testStyle?: object }) => ({ display: "block", visibility: "visible", opacity: "1", ...el.testStyle }),
    setTimeout: (callback: () => void) => callback(),
  });
  return { result: sanitizePlatformSetupResult(result), created };
}

test('唯一工作空间自动选择提交，多个候选暂停；人工选好后继续提交原表单', async () => {
  assert.deepEqual(await continueTunnel(['Only workspace']), { result: { status: 'ready', tunnel_id: 'tunnel_created' }, created: 1 });
  assert.deepEqual(await continueTunnel(['Only workspace'], false, true), { result: { status: 'ready', tunnel_id: 'tunnel_created' }, created: 1 });
  assert.deepEqual(await continueTunnel(['A', 'B']), { result: { status: 'workspace_required' }, created: 0 });
  assert.deepEqual(await continueTunnel(['A', 'B'], true), { result: { status: 'ready', tunnel_id: 'tunnel_created' }, created: 1 });
});
