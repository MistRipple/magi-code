import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const repositoryRoot = path.resolve(new URL('../..', import.meta.url).pathname);
const read = (relative) => fs.readFileSync(path.join(repositoryRoot, relative), 'utf8');

const section = read('web/src/components/SettingsMcpServerSection.svelte');
const tray = read('web/src/components/ExternalApprovalTray.svelte');
const toolsTab = read('web/src/components/SettingsToolsTab.svelte');
const shell = read('web/src/web/WebWorkbenchShell.svelte');
const bridge = read('web/src/shared/bridges/web-client-bridge.ts');
const agentApi = read('web/src/web/agent-api.ts');
const events = read('web/src/lib/mcp-server-events.ts');
const zhCN = JSON.parse(read('web/src/i18n/zh-CN.json'));
const enUS = JSON.parse(read('web/src/i18n/en-US.json'));

// ── 前端只消费 daemon 事实：不落任何存储 ─────────────────────────────────────
for (const [name, source] of [['section', section], ['tray', tray]]) {
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

// ── 令牌可重新查看 / 编辑 / 重新生成；旧令牌没有原文时只能重新生成 ─────────────
assert.match(section, /getMcpServerTokenSecret\(token\.tokenId\)/);
assert.match(section, /token\.hasSecret/);
assert.match(section, /data-mcp-token-legacy/);
assert.match(section, /rotateMcpServerToken\(token\.tokenId\)/);
assert.match(section, /window\.confirm\(i18n\.t\('mcpServer\.token\.rotateConfirm'/);
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
assert.match(section, /window\.confirm\(i18n\.t\('mcpServer\.network\.confirm'\)\)/);
assert.match(section, /setMcpServerNetwork\(enabled, enabled\)/);
assert.match(section, /allowNetwork: allowNetwork && !needsConfirm \? true : undefined/);
assert.match(section, /if \(needsConfirm\) allowNetwork = false/);
assert.match(section, /!status\.network\.enabled && !hasNetworkToken/);
assert.match(section, /mcpServer\.network\.addressChanges/);
assert.match(agentApi, /body: \{ enabled, confirmRisk \}/);

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
for (const source of [section, tray]) {
  for (const match of source.matchAll(/i18n\.t\(\s*['`](mcpServer\.[^'`$]+)['`]/g)) usedKeys.add(match[1]);
}
for (const profile of ['read_only', 'edit', 'edit_trusted', 'exec']) {
  usedKeys.add(`mcpServer.token.profile.${profile}`);
  usedKeys.add(`mcpServer.token.profileShort.${profile}`);
}
for (const state of ['stopped', 'installing', 'starting', 'running', 'error']) {
  usedKeys.add(`mcpServer.network.status.${state}`);
}
for (const outcome of ['succeeded', 'failed', 'denied']) usedKeys.add(`mcpServer.audit.outcome.${outcome}`);
for (const key of usedKeys) {
  assert.ok(key in zhCN, `zh-CN 缺少 ${key}`);
  assert.ok(key in enUS, `en-US 缺少 ${key}`);
}
const mcpKeys = (dict) => Object.keys(dict).filter((key) => key.startsWith('mcpServer.')).sort();
assert.deepEqual(mcpKeys(zhCN), mcpKeys(enUS), '中英文 mcpServer.* 键必须一一对应');

console.log('mcp-server golden ok');
