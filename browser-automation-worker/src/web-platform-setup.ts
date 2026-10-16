/** 平台配置只返回固定状态与非机密 Tunnel id，不能返回正文或任意表单值。 */
export const PLATFORM_SETUP_SCRIPT = String.raw`(async (action) => {
  if (location.origin !== 'https://platform.openai.com') return {status:'login_required'};
  if (!location.pathname.startsWith('/settings/organization/tunnels')) return {status:'page_required'};
  const visible = el => el.getClientRects().length > 0;
  const nodes = (selector, root = document) => [...root.querySelectorAll(selector)].filter(visible);
  const text = el => (el.textContent || '').trim();
  const button = (re, root = document) => nodes('button, [role="button"]', root).find(el => re.test(text(el)));
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const input = globalThis.__magiSetupInput;
  const press = el => input.perform('click', el);
  const inspect = () => {
    if (nodes('input[type="password"], input[autocomplete="one-time-code"]').length) return {status:'login_required'};
    const rows = nodes('tr, [role="row"], [data-testid="tunnel-row"]')
      .filter(row => nodes('td, [role="cell"], [data-testid="tunnel-name"]', row).some(cell => text(cell) === 'Magi'));
    if (rows.length > 1) return {status:'ambiguous_tunnel'};
    if (rows.length === 1) {
      // textContent 会把相邻单元格拼成 Magitunnel_…，必须按单元格独立识别。
      const ids = [...new Set(nodes('td, [role="cell"], [data-testid="tunnel-id"]', rows[0])
        .flatMap(cell => text(cell).match(/\btunnel_[A-Za-z0-9_-]+\b/gu) || []))];
      const id = ids.length === 1 ? ids[0] : null;
      return id ? {status:'ready',tunnel_id:id} : {status:'tunnel_id_required'};
    }
    return button(/^(?:Create tunnel|Create a tunnel|New tunnel|创建隧道|新建隧道)$/iu)
      ? {status:'missing'} : {status:'permission_or_page_required'};
  };
  let before = inspect();
  for (let i=0; i<10 && before.status==='permission_or_page_required'; i++) { await sleep(300); before=inspect(); }
  if (action==='inspect_tunnel' || before.status!=='missing') return before;
  const create=button(/^(?:Create tunnel|Create a tunnel|New tunnel|创建隧道|新建隧道)$/iu);
  if (!create) return {status:'manual_required'};
  await press(create); await sleep(300);
  const dialogs=nodes('[role="dialog"], dialog');
  if (dialogs.length!==1) return {status:'manual_required'};
  const form=dialogs[0];
  const inputs=nodes('input[type="text"], input:not([type])', form);
  if (inputs.length!==1 || nodes('input[type="password"]',form).length) return {status:'manual_required'};
  await input.perform('type', inputs[0], 'Magi');
  const save=button(/^(?:Create|Create tunnel|创建|创建隧道)$/iu,form);
  if (!save || save.disabled) return {status:'manual_required'};
  await press(save);
  for (let i=0;i<30;i++) { await sleep(300); const after=inspect(); if (after.status==='ready') return after; }
  return {status:'creation_unconfirmed'};
})`;

export const PLATFORM_SETUP_STATUSES = new Set([
  'login_required', 'page_required', 'ambiguous_tunnel', 'ready', 'tunnel_id_required',
  'missing', 'permission_or_page_required', 'manual_required', 'creation_unconfirmed',
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
