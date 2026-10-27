/** 平台配置只返回固定状态与非机密 Tunnel id，不能返回正文或任意表单值。 */
export const PLATFORM_SETUP_SCRIPT = String.raw`(async (action) => {
  if (location.origin !== 'https://platform.openai.com') return {status:'login_required'};
  const visible = el => {
    const style = getComputedStyle(el);
    const rect = el.getBoundingClientRect();
    return el.getClientRects().length > 0 && rect.width > 0 && rect.height > 0
      && style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity || '1') > 0
      && el.getAttribute('aria-hidden') !== 'true';
  };
  const nodes = (selector, root = document) => [...root.querySelectorAll(selector)].filter(visible);
  const text = el => (el.textContent || '').replace(/\s+/gu, ' ').trim();
  const label = el => el.getAttribute('aria-label')
    || (el.getAttribute('aria-labelledby') || '').split(/\s+/u).filter(Boolean).map(id => document.getElementById(id)).filter(Boolean).map(text).join(' ')
    || el.getAttribute('title') || (el.labels?.length ? [...el.labels].map(text).join(' ').trim() : text(el));
  const button = (re, root = document) => nodes('button, [role="button"]', root).find(el => !el.disabled && el.getAttribute('aria-disabled')!=='true' && re.test(label(el)));
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const waitFor = async read => {
    for (let i=0;i<30;i++) { const value=read(); if (value) return value; await sleep(300); }
    return null;
  };
  const input = globalThis.__magiSetupInput;
  const press = el => input.perform('click', el);
  const dialogs = () => nodes('[role="dialog"], dialog');
  // Only a unique visible choice may be selected; multiple scopes require a human.
  const choose = async (control, preferred, multiple = false) => {
    if (!control) return false;
    if (control.tagName === 'SELECT') {
      const options = [...control.options].filter(option => !option.disabled && option.value && !/^(?:Select|Choose|选择)/iu.test(text(option)));
      const matched = preferred ? options.filter(option => preferred.test(text(option))) : options;
      if (matched.length !== 1) return false;
      await input.perform('select', control, matched[0].value);
      return control.value === matched[0].value;
    }
    await press(control);
    const options = await waitFor(() => {
      const values = nodes('[role="option"]').filter(el => el.getAttribute('aria-disabled') !== 'true');
      return values.length ? values : null;
    });
    const matched = options && (preferred ? options.filter(el => preferred.test(text(el))) : options);
    if (!matched || matched.length !== 1) {
      // Close only the menu we opened, leaving the form available for the user.
      await press(control);
      return false;
    }
    const value = text(matched[0]);
    await press(matched[0]);
    // 工作空间是多选菜单，选中唯一项后仍保持打开；关闭本次打开的菜单，不重复点击选项。
    if (multiple && nodes('[role="listbox"]').length) await press(control);
    // 单选菜单选择后自行关闭；退出动画期间仍有布局盒，不能再次点击触发器。
    if (!await waitFor(() => !nodes('[role="listbox"]').length)) return false;
    return await waitFor(() => text(control).includes(value) || label(control).includes(value)) !== null;
  };
  const keyForm = async () => {
    if (nodes('input[type="password"], input[autocomplete="one-time-code"]').length) return {status:'login_required'};
    let form = dialogs()[0];
    if (!form) {
      const create = await waitFor(() => button(/^(?:Create new secret key|创建新密钥)$/iu));
      if (!create) return {status:'manual_required'};
      await press(create);
      form = await waitFor(() => dialogs().length === 1 ? dialogs()[0] : null);
    }
    if (!form || !/Create new secret key|创建新密钥/iu.test(text(nodes('h1,h2,h3', form)[0] || {}))) return {status:'manual_required'};
    const name = await waitFor(() => nodes('input[type="text"], input:not([type])', form).find(el => /My Test Key|name|名称/iu.test(el.getAttribute('placeholder') || el.getAttribute('name') || el.getAttribute('aria-label') || '')));
    if (!name) return {status:'manual_required'};
    if (!name.value?.trim()) await input.perform('type', name, 'Magi Tunnel Runtime');
    // Keep an already chosen project. Never switch a user's selected project.
    const fieldControl = re => {
      const title = nodes('label, span, div', form).find(el => re.test(text(el)));
      if (!title) return null;
      if (title.control && visible(title.control)) return title.control;
      let parent = title.parentElement;
      for (let i=0; parent && parent !== form && i<3; i++, parent=parent.parentElement) {
        const controls = nodes('button, select, [role="button"], [role="combobox"], [aria-haspopup="listbox"]', parent);
        if (controls.length === 1) return controls[0];
        if (controls.length > 1) break;
      }
      return null;
    };
    const project = await waitFor(() => {
      const control = nodes('button, select, [role="button"], [role="combobox"], [aria-haspopup="listbox"]', form)
        .find(el => /^(?:Select project\.{0,3}|选择项目|Default project)$/iu.test(text(el))) || fieldControl(/^(?:Project|项目)$/iu);
      return control && !control.disabled && control.getAttribute('aria-disabled') !== 'true'
        && !/Loading|加载/iu.test(text(control) + ' ' + label(control)) ? control : null;
    });
    if (!project || (/Select|选择/iu.test(text(project) + ' ' + label(project)) && !await choose(project, null))) return {status:'project_required'};
    // Locate the expiry control by its own accessible name, not a positional index.
    // 选定项目后平台异步插入有效期字段，项目名称已回读不代表依赖字段已就绪。
    const expiry = await waitFor(() => {
      const current = dialogs();
      if (current.length !== 1) return null;
      form = current[0];
      const control = nodes('button, select, [role="button"], [role="combobox"], [aria-haspopup="listbox"]', form).find(el => /有效期|Expiration|Expires|Expiry/iu.test(label(el))) || fieldControl(/^(?:有效期|Expiration|Expires|Expiry)$/iu);
      return control && !control.disabled && control.getAttribute('aria-disabled') !== 'true' ? control : null;
    });
    const expiryValue = () => expiry?.tagName === 'SELECT' ? [...expiry.selectedOptions].map(text).join(' ') : expiry && label(expiry);
    if (!expiry || (!/30\s*(?:天|days?)/iu.test(expiryValue()) && !await choose(expiry, /^(?:30\s*(?:天|days?))$/iu))) return {status:'manual_required'};
    const restricted = nodes('input[type="radio"], [role="radio"]', form).find(el => /^(?:Restricted|受限)$/iu.test(label(el)));
    if (!restricted) return {status:'key_permissions_required'};
    if (!restricted.checked && restricted.getAttribute('aria-checked') !== 'true') await press(restricted);
    const tunnelLabel = await waitFor(() => nodes('button', form).find(el => /^(?:Tunnels|隧道)$/iu.test(text(el))));
    if (!tunnelLabel) return {status:'key_permissions_required'};
    // Permission rows use a disabled resource label followed by its select button.
    const controls = nodes('button, [role="button"], [role="combobox"]', form);
    const tunnelIndex = controls.indexOf(tunnelLabel);
    const permission = controls[tunnelIndex + 1];
    // 平台用 aria-labelledby 标注资源名，可见的选择值才是 None / All selected。
    const permissionValue = () => permission && text(permission);
    if (!permission || !/^(?:None|Read|Use|All selected|无|全部已选)$/iu.test(permissionValue())) return {status:'key_permissions_required'};
    await press(permission);
    const choices = await waitFor(() => {
      const values = nodes('[role="option"]');
      return values.length === 2 && values.every(el => /^(?:Read|Use)$/u.test(text(el))) ? values : null;
    });
    if (!choices) return {status:'key_permissions_required'};
    for (const value of ['Read', 'Use']) {
      const el = nodes('[role="option"]').find(el => text(el) === value);
      if (!el) return {status:'key_permissions_required'};
      if (el.getAttribute('aria-selected') !== 'true') await press(el);
    }
    if (nodes('[role="listbox"]').length) await press(permission);
    // Confirm the displayed aggregate as well as the Tunnel selector. No key is generated or read.
    const selected = await waitFor(() => nodes('span, p, div', form).some(el => /^2 selected permissions?$/u.test(text(el))));
    if (!selected || !/^(?:All selected|全部已选)$/iu.test(permissionValue())) return {status:'key_permissions_required'};
    return {status:'key_generation_required'};
  };
  if (action === 'prepare_runtime_key') {
    if (!location.pathname.startsWith('/settings/organization/api-keys')) return {status:'page_required'};
    return await keyForm();
  }
  if (!location.pathname.startsWith('/settings/organization/tunnels')) return {status:'page_required'};
  const rows = () => nodes('tr, [role="row"], [data-testid="tunnel-row"]')
    .filter(row => nodes('td, [role="cell"], [data-testid="tunnel-name"]', row).length);
  const rowIds = row => [...new Set(nodes('td, [role="cell"], [data-testid="tunnel-id"]', row)
    .flatMap(cell => text(cell).match(/\btunnel_[A-Za-z0-9_-]{1,160}\b/gu) || []))];
  const inspect = () => {
    if (nodes('input[type="password"], input[autocomplete="one-time-code"]').length) return {status:'login_required'};
    if (document.readyState === 'loading' || nodes('[aria-busy="true"], [role="progressbar"]').length) return {status:'permission_or_page_required'};
    if (nodes('[role="dialog"], dialog').length) return {status:'manual_required'};
    const namedRows = nodes('tr, [role="row"], [data-testid="tunnel-row"]')
      .filter(row => nodes('td, [role="cell"], [data-testid="tunnel-name"]', row).some(cell => text(cell) === 'Magi'));
    if (namedRows.length > 1) return {status:'ambiguous_tunnel'};
    if (namedRows.length === 1) {
      // textContent 会把相邻单元格拼成 Magitunnel_…，必须按单元格独立识别。
      const ids = rowIds(namedRows[0]);
      const id = ids.length === 1 ? ids[0] : null;
      return id ? {status:'ready',tunnel_id:id} : {status:'tunnel_id_required'};
    }
    const dataRows = rows();
    // 新平台列表只显示 ID 与操作，不显示名称；不能据此断言 Magi 不存在并重复创建。
    if (dataRows.length && !nodes('th, [role="columnheader"]').some(el => /^(?:Name|名称)$/iu.test(text(el)))) return {status:'tunnel_id_required'};
    const listLoaded = (dataRows.length > 0 && dataRows.every(row =>
      nodes('td, [role="cell"], [data-testid="tunnel-id"]', row).some(cell => /\btunnel_[A-Za-z0-9_-]+\b/u.test(text(cell)))))
      || nodes('h1, h2, h3, p, span, div').some(el => /^(?:No tunnels yet|暂无隧道|尚无隧道)$/iu.test(text(el)));
    if (!listLoaded) return {status:'permission_or_page_required'};
    return button(/^(?:Create tunnel|Create a tunnel|New tunnel|创建隧道|新建隧道)$/iu)
      ? {status:'missing'} : {status:'permission_or_page_required'};
  };
  // ID-only 列表通过同一行的 Edit 详情核对名称，绝不把唯一一行直接当作 Magi。
  // 只打开并取消详情，不保存；页面变化或详情不明时保留人工处理边界。
  const inspectDetails = async result => {
    if (result.status !== 'tunnel_id_required' || nodes('[role="dialog"], dialog').length) return result;
    const initialRows = rows();
    if (!initialRows.length || initialRows.length > 4) return result;
    const ids = initialRows.map(row => rowIds(row));
    if (ids.some(value => value.length !== 1) || new Set(ids.flat()).size !== ids.length) return result;
    const matches = [];
    for (const [id] of ids) {
      const row = rows().find(row => rowIds(row).length === 1 && rowIds(row)[0] === id);
      const edit = row && button(/^(?:Edit tunnel|编辑隧道)$/iu, row);
      if (!edit) return result;
      await press(edit);
      const form = await waitFor(() => {
        const dialogs = nodes('[role="dialog"], dialog');
        return dialogs.length === 1 && /^(?:Edit tunnel|编辑隧道)$/iu.test(text(nodes('h1,h2,h3', dialogs[0])[0] || {})) ? dialogs[0] : null;
      });
      if (!form || nodes('input[type="password"]', form).length) return {status:'manual_required'};
      const name = await waitFor(() => {
        const fields = nodes('input[type="text"], input:not([type])', form);
        return fields.length === 1 && fields[0].value?.trim() ? fields[0].value.trim() : null;
      });
      const cancel = button(/^(?:Cancel|取消)$/iu, form);
      if (!cancel) return {status:'manual_required'};
      await press(cancel);
      if (!await waitFor(() => !nodes('[role="dialog"], dialog').length)) return {status:'manual_required'};
      if (!name) return result;
      if (name === 'Magi') matches.push(id);
      if (matches.length > 1) return {status:'ambiguous_tunnel'};
    }
    const after = inspect();
    const finalIds = rows().flatMap(row => rowIds(row));
    if (after.status !== 'tunnel_id_required' || finalIds.length !== ids.length || !ids.every(([id]) => finalIds.includes(id))) return result;
    return matches.length === 1 ? {status:'ready',tunnel_id:matches[0]} : result;
  };
  let form = null;
  if (action === 'create_tunnel' && dialogs().length === 1) {
    const candidate = dialogs()[0];
    const heading = text(nodes('h1,h2,h3', candidate)[0] || {});
    if (/^(?:Create tunnel|创建隧道)$/iu.test(heading)) form = candidate;
  }
  if (!form) {
    let before = inspect();
    for (let i=0; i<30 && before.status==='permission_or_page_required'; i++) { await sleep(300); before=inspect(); }
    before = await inspectDetails(before);
    if (action==='inspect_tunnel' || before.status!=='missing') return before;
    const create=button(/^(?:Create tunnel|Create a tunnel|New tunnel|创建隧道|新建隧道)$/iu);
    if (!create) return {status:'manual_required'};
    await press(create);
    form=await waitFor(() => {
      const values=dialogs();
      return values.length===1 && nodes('input[type="text"], input:not([type])', values[0]).length===1 ? values[0] : null;
    });
  }
  if (!form || nodes('input[type="password"]',form).length) return {status:'manual_required'};
  const inputs=nodes('input[type="text"], input:not([type])', form);
  if (inputs.length !== 1 || (inputs[0].value?.trim() && inputs[0].value.trim() !== 'Magi')) return {status:'manual_required'};
  if (!inputs[0].value?.trim()) await input.perform('type', inputs[0], 'Magi');
  const descriptions=nodes('textarea', form);
  if (descriptions.length===1 && !descriptions[0].value?.trim()) await input.perform('type', descriptions[0], 'Connect this ChatGPT workspace to Magi tools through the local Magi client.');
  const workspace = button(/^(?:Search workspaces by name or ID|搜索工作空间名称或 ID)$/iu,form);
  if (workspace && !await choose(workspace, null, true)) return {status:'workspace_required'};
  const save=await waitFor(() => {
    const el=button(/^(?:Create|Create tunnel|创建|创建隧道)$/iu,form);
    return el && !el.disabled && el.getAttribute('aria-disabled')!=='true' ? el : null;
  });
  if (!save) return {status:'manual_required'};
  await press(save);
  for (let i=0;i<30;i++) { await sleep(300); const after=await inspectDetails(inspect()); if (after.status==='ready' || after.status==='ambiguous_tunnel') return after; }
  return {status:'creation_unconfirmed'};
})`;

export const PLATFORM_SETUP_STATUSES = new Set([
  'login_required', 'page_required', 'ambiguous_tunnel', 'ready', 'tunnel_id_required',
  'missing', 'permission_or_page_required', 'manual_required', 'creation_unconfirmed', 'workspace_required',
  'key_generation_required', 'project_required', 'key_permissions_required',
]);

export function sanitizePlatformSetupResult(value: unknown): { status: string; tunnel_id?: string } | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  if (typeof record.status !== 'string' || !PLATFORM_SETUP_STATUSES.has(record.status)) return null;
  if (record.status === 'ready') {
    if (typeof record.tunnel_id !== 'string' || !/^tunnel_[A-Za-z0-9_-]{1,160}$/u.test(record.tunnel_id)) return null;
    return { status: record.status, tunnel_id: record.tunnel_id };
  }
  return { status: record.status };
}
