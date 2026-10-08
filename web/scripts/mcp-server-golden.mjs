import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const repositoryRoot = path.resolve(new URL('../..', import.meta.url).pathname);
const read = (relative) => fs.readFileSync(path.join(repositoryRoot, relative), 'utf8');

const section = read('web/src/components/SettingsMcpServerSection.svelte');
const audit = read('web/src/components/SettingsMcpServerAudit.svelte');
const tray = read('web/src/components/ExternalApprovalTray.svelte');
const toolsTab = read('web/src/components/SettingsToolsTab.svelte');
const shell = read('web/src/web/WebWorkbenchShell.svelte');
const bridge = read('web/src/shared/bridges/web-client-bridge.ts');
const agentApi = read('web/src/web/agent-api.ts');
const events = read('web/src/lib/mcp-server-events.ts');
const zhCN = JSON.parse(read('web/src/i18n/zh-CN.json'));
const enUS = JSON.parse(read('web/src/i18n/en-US.json'));

// ── 前端只消费 daemon 事实：不落任何存储 ─────────────────────────────────────
for (const [name, source] of [['section', section], ['tray', tray], ['audit', audit]]) {
  assert.doesNotMatch(source, /localStorage|sessionStorage|indexedDB/, `${name} 不得持久化 MCP 数据`);
}
assert.match(section, /freshSecret = ''/, '令牌原文在用户确认后必须丢弃');
assert.doesNotMatch(section, /secret[^\n]*(localStorage|console\.)/);

// ── API 路径与 daemon 路由一致 ────────────────────────────────────────────────
for (const route of [
  '/api/mcp-server/status',
  '/api/mcp-server/enabled',
  '/api/mcp-server/network',
  '/api/mcp-server/tokens',
  '/api/mcp-server/tokens/revoke-all',
  '/api/mcp-server/config-snippets',
  '/api/mcp-server/approvals',
  '/api/mcp-server/approvals/resolve',
  '/api/mcp-server/audit',
]) {
  assert.ok(agentApi.includes(route), `agent-api 缺少 ${route}`);
}
assert.match(agentApi, /encodeURIComponent\(tokenId\)/);
assert.match(agentApi, /\/secret`/, 'agent-api 缺少查看原文');
assert.match(agentApi, /\/rotate`/, 'agent-api 缺少重新生成');
assert.match(agentApi, /method: 'PATCH', body: request/, 'agent-api 缺少编辑令牌');
assert.match(agentApi, /config-snippets\$\{suffix\}/, '配置片段需要支持按令牌取');

// ── 令牌可重新查看 / 编辑 / 重新生成；没有原文的令牌只能重新生成 ─────────────
assert.match(section, /getMcpServerTokenSecret\(token\.tokenId\)/);
assert.match(section, /token\.hasSecret/);
assert.match(section, /data-mcp-token-no-secret/);
assert.match(section, /rotateMcpServerToken\(token\.tokenId\)/);
assert.match(section, /confirmMessage\(i18n\.t\('mcpServer\.token\.rotateConfirm'/);
// 编辑：高风险档要确认；升到高风险档时网络一并关闭；工作区不可改
assert.match(section, /confirmHighRisk: editNeedsConfirm \? editConfirmHighRisk : undefined/);
assert.match(section, /const network = editHighRisk \? false : editNetwork/);
assert.doesNotMatch(section, /updateMcpServerToken\([^)]*workspaceId/);
assert.match(section, /viewingSecret = null/, '收起后原文必须丢弃');

// ── 高风险权限档必须显式确认 ─────────────────────────────────────────────────
assert.match(section, /HIGH_RISK_PROFILES[\s\S]*'edit_trusted'[\s\S]*'exec'/);
assert.match(section, /\(!needsConfirm \|\| confirmHighRisk\)/);
assert.match(section, /confirmHighRisk: needsConfirm \? confirmHighRisk : undefined/);

// ── 网络模式：需要确认风险；高风险权限档不能带网络；地址会变要提示 ─────────────
assert.match(section, /confirmMessage\(i18n\.t\('mcpServer\.network\.confirm'\)/);
assert.match(section, /setMcpServerNetwork\(enabled, enabled\)/);
assert.match(section, /allowNetwork: allowNetwork && !needsConfirm \? true : undefined/);
assert.match(section, /if \(needsConfirm\) allowNetwork = false/);
assert.match(section, /!status\.network\.enabled && !hasNetworkToken/);
assert.match(section, /mcpServer\.network\.addressChanges/);
assert.match(agentApi, /body: \{ enabled, confirmRisk \}/);

// ── 命名隧道：令牌只在提交时发给 daemon，提交后立即清空，不回显、不存储 ───────────
assert.ok(agentApi.includes('/api/mcp-server/named-tunnel'));
assert.ok(agentApi.includes('/api/mcp-server/named-tunnel/verify'));
assert.match(agentApi, /method: 'PUT',\s*body: \{ hostname, token \}/);
assert.match(section, /namedToken = ''/, '命名隧道令牌提交后必须清空');
assert.match(section, /type="password" bind:value=\{namedToken\}/, '令牌输入框必须是密码框');
assert.match(zhCN['mcpServer.named.assist'], /\/magi-cloudflare-tunnel/, '设置页需提示内置 Skill 入口');
assert.match(enUS['mcpServer.named.assist'], /\/magi-cloudflare-tunnel/);
assert.doesNotMatch(section, /namedToken[^\n]*(localStorage|console\.)/);

// ── 隧道就绪后的“添加到远程客户端”：表单字段 / JSON / 命令一键复制，令牌来自 daemon ──
assert.match(section, /data-mcp-quick-add="1"/);
assert.match(section, /\{#if remoteReady\}/, '隧道就绪或公网直连在监听时才显示');
assert.match(section, /getMcpServerConfigSnippets\(token\.tokenId\)/);
assert.match(section, /remoteClaudeCli/);
assert.match(agentApi, /remoteClaudeCli: string \| null/);

// ── 内置技能：可停用，不显示删除按钮（后端同样拒绝移除） ───────────────────────
const toolsTabSource = toolsTab;
const settingsStore = read('web/src/stores/settings-store.svelte.ts');
assert.match(toolsTabSource, /\{#if skill\.builtin\}[\s\S]*?data-skill-builtin[\s\S]*?\{:else\}[\s\S]*?deleteSkill\(skill\)/);
assert.equal((settingsStore.match(/builtin: skill\.builtin === true/g) ?? []).length, 2, '两处构造技能列表都要带 builtin');

// ── 面板按用途分组；调用记录分页并可清理，数据来自 daemon ─────────────────────
for (const id of ['connect', 'clients', 'remote', 'activity']) assert.match(section, new RegExp(`'${id}'`));
assert.match(section, /<SettingsMcpServerAudit /);
assert.match(section, /tab === 'activity'/);
assert.match(section, /data-mcp-create-open/, '新建令牌表单默认收起');
assert.match(audit, /const PAGE_SIZE = 20/);
assert.match(audit, /offset,\s*tokenId: filterTokenId \|\| undefined/);
assert.match(audit, /clearMcpServerAudit\(filterTokenId \|\| undefined\)/);
assert.match(audit, /confirmMessage\(message/, '清理前必须确认');
assert.match(agentApi, /clear mcp server audit/);
assert.match(agentApi, /if \(options\.offset\) query\.set\('offset'/);

// ── 令牌来源要说清楚：配置卡片选客户端自动填入，没有令牌时直接引导去创建 ──────────
assert.match(section, /data-mcp-token-origin/);
assert.match(section, /data-mcp-connect-token/);
assert.match(section, /data-mcp-go-clients="1"[\s\S]*?tab = 'clients'; showCreate = true/);
assert.match(section, /data-mcp-placeholder-note/);
assert.match(zhCN['mcpServer.snippets.hint'], /“客户端”分组/);
assert.match(zhCN['mcpServer.network.needToken'], /“客户端”分组/);
assert.match(zhCN['mcpServer.snippets.placeholderNote'], /MAGI_MCP_TOKEN/);

// ── 直接对公网开放（监听公网 IP / 0.0.0.0）：必须确认明文风险，只引用 daemon 的状态 ──
assert.ok(agentApi.includes('/api/mcp-server/direct'));
assert.match(agentApi, /method: 'PUT',\s*body: request/);
assert.match(section, /data-mcp-server-direct="1"/);
assert.match(section, /confirmMessage\(i18n\.t\('mcpServer\.direct\.confirm'\)/);
assert.match(section, /confirmRisk: true/);
assert.match(section, /data-mcp-direct-risk/, '页面必须展示明文传输的风险');
assert.match(zhCN['mcpServer.direct.risk'], /明文 HTTP/);
assert.match(zhCN['mcpServer.direct.risk'], /TLS/);
assert.match(section, /canStartDirect/);
assert.match(section, /hasNetworkToken/);

// ── 外部审批走跨会话托盘，不能污染当前会话的授权投影 ─────────────────────────
assert.match(events, /MCP_APPROVALS_CHANGED_EVENT = 'magi:mcp-approvals-changed'/);
assert.match(
  bridge,
  /event\.payload\?\.external === true\) \{[\s\S]*?MCP_APPROVALS_CHANGED_EVENT[\s\S]*?return;[\s\S]*?\}\s*refreshCurrentSessionToolApprovals\(eventType\)/,
);
assert.match(shell, /<ExternalApprovalTray \/>/);
assert.match(toolsTab, /<SettingsMcpServerSection \/>/);

// ── 托盘：没有待确认项时不产生周期请求；无管理权限时自行停用 ─────────────────
assert.match(tray, /if \(approvals\.length > 0\) void refresh\(\)/);
assert.match(tray, /disabled = true;/);
assert.match(tray, /'allow_once'/);
assert.doesNotMatch(tray, /allow_for_turn/);

// ── i18n：用到的键中英文都存在，且两边键集合一致 ─────────────────────────────
const usedKeys = new Set();
for (const action of ['token.view_secret', 'token.update', 'token.rotate']) usedKeys.add(`mcpServer.audit.action.${action}`);
for (const id of ['connect', 'clients', 'remote', 'activity']) usedKeys.add(`mcpServer.tab.${id}`);
for (const source of [section, tray, audit]) {
  for (const match of source.matchAll(/i18n\.t\(\s*['`](mcpServer\.[^'`$]+)['`]/g)) usedKeys.add(match[1]);
}
for (const profile of ['read_only', 'edit', 'edit_trusted', 'exec']) {
  usedKeys.add(`mcpServer.token.profile.${profile}`);
  usedKeys.add(`mcpServer.token.profileShort.${profile}`);
}
for (const state of ['stopped', 'installing', 'starting', 'running', 'error']) {
  usedKeys.add(`mcpServer.network.status.${state}`);
}
for (const code of ['ok', 'not_configured', 'network_off', 'dns_unresolved', 'tunnel_not_connected', 'origin_unreachable', 'unexpected']) {
  usedKeys.add(`mcpServer.named.verifyResult.${code}`);
}
for (const code of ['tunnel_dependency_unavailable', 'tunnel_start_failed', 'tunnel_connection_lost', 'tunnel_token_invalid']) {
  usedKeys.add(`mcpServer.network.error.${code}`);
}
for (const key of ['section', 'desc', 'token', 'form', 'fieldUrl', 'fieldHeaderName', 'fieldHeaderValue', 'json', 'fixedHint']) {
  usedKeys.add(`mcpServer.quickAdd.${key}`);
}
for (const outcome of ['succeeded', 'failed', 'denied']) usedKeys.add(`mcpServer.audit.outcome.${outcome}`);
for (const key of usedKeys) {
  assert.ok(key in zhCN, `zh-CN 缺少 ${key}`);
  assert.ok(key in enUS, `en-US 缺少 ${key}`);
}
const mcpKeys = (dict) => Object.keys(dict).filter((key) => key.startsWith('mcpServer.')).sort();
assert.deepEqual(mcpKeys(zhCN), mcpKeys(enUS), '中英文 mcpServer.* 键必须一一对应');

console.log('mcp-server golden ok');
