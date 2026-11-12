import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
import { parse } from 'svelte/compiler';
import { readSplitSource } from './lib/split-source.mjs';

const repositoryRoot = path.resolve(new URL('../..', import.meta.url).pathname);
const read = (relative) => fs.readFileSync(path.join(repositoryRoot, relative), 'utf8');

const section = read('web/src/components/SettingsMcpServerSection.svelte');
const audit = read('web/src/components/SettingsMcpServerAudit.svelte');
const tray = read('web/src/components/ExternalApprovalTray.svelte');
const approvalStore = read('web/src/stores/external-approvals.svelte.ts');
const approvalActions = read('web/src/components/ExternalApprovalActions.svelte');
const settingsPanel = read('web/src/components/SettingsPanel.svelte');
const toolsTab = read('web/src/components/SettingsToolsTab.svelte');
const shell = read('web/src/web/WebWorkbenchShell.svelte');
const bridge = read('web/src/shared/bridges/web-client-bridge.ts');
const agentApi = readSplitSource(path.join(repositoryRoot, 'web/src/web/agent-api.ts'));
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
// 所有客户端使用完整工具目录，选择每次询问、允许读写或完全访问。
assert.doesNotMatch(section, /McpServerProfile|data-mcp-profile|data-mcp-edit-profile|confirmHighRisk|editConfirmHighRisk/);
assert.doesNotMatch(agentApi, /McpServerProfile|confirmHighRisk/);
assert.doesNotMatch(section, /updateMcpServerToken\([^)]*workspaceId/);
assert.match(section, /viewingSecret = null/, '收起后原文必须丢弃');

// ── MCP 权限分为每次询问、允许读写和完全访问三档 ───────────────────────────────────────
assert.match(section, /data-mcp-approval-mode/);
assert.match(section, /data-mcp-edit-approval-mode/);
assert.match(section, /const APPROVAL_MODES:[^\n]*= \['ask', 'read_write', 'always'\]/);
assert.doesNotMatch(section, /profile !== 'read_only'/);
assert.match(section, /let approvalMode = \$state<McpServerApprovalMode>\('ask'\)/);

// ── 网络模式：需要确认风险；工具目录保持完整；地址会变要提示 ──────────────────
assert.match(section, /confirmMessage\(i18n\.t\('mcpServer\.network\.confirm'\)/);
assert.match(section, /setMcpServerNetwork\(enabled, enabled\)/);
assert.match(section, /allowNetwork: allowNetwork \? true : undefined/);
assert.match(section, /data-mcp-allow-network="1"/);
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
for (const id of ['clients', 'remote', 'activity']) assert.match(section, new RegExp(`'${id}'`));
assert.match(section, /<SettingsMcpServerAudit /);
assert.match(section, /tab === 'activity'/);
assert.match(section, /data-mcp-create-open/, '新建令牌表单默认收起');
assert.match(audit, /const PAGE_SIZE = 20/);
assert.match(audit, /offset,\s*tokenId: filterTokenId \|\| undefined/);
assert.match(audit, /clearMcpServerAudit\(filterTokenId \|\| undefined\)/);
assert.match(audit, /confirmMessage\(message/, '清理前必须确认');
assert.match(agentApi, /clear mcp server audit/);
assert.match(agentApi, /if \(options\.offset\) query\.set\('offset'/);

// ── 连接配置归属对应客户端；网络入口可直接进入新建客户端流程 ──────────────────
assert.match(section, /data-mcp-token-view=\{token\.tokenId\}/);
assert.match(section, /data-mcp-token-detail=\{token\.tokenId\}/);
assert.match(section, /data-mcp-go-clients-network="1"[\s\S]*?openCreateToken\(true\)/);
assert.match(zhCN['mcpServer.network.needToken'], /“客户端”分组/);

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
  /event\.payload\?\.external === true\) \{\s*window\.dispatchEvent\(new CustomEvent\(MCP_APPROVALS_CHANGED_EVENT\)\);\s*if \(rustEventSessionId\(event\) === currentSessionId\.trim\(\)\) \{\s*refreshCurrentSessionToolApprovals\(eventType\);\s*\}\s*return;/,
);
assert.match(shell, /<ExternalApprovalTray \/>/);
assert.match(shell, /mergeExternalSessionsIntoSidebar/);
assert.doesNotMatch(shell, /<ExternalSessionsSection/);
// MCP 服务是始终可见的原生设置分类，不依赖插件激活。
assert.match(settingsPanel, /<SettingsMcpServerSection \/>/);
assert.doesNotMatch(toolsTab, /SettingsMcpServerSection/);

// ── 托盘：没有待确认项时不产生周期请求；无管理权限时自行停用 ─────────────────
// 托盘与外部会话里的调用卡片共用同一个存储和同一组操作按钮：批准与拒绝只有一个入口。
assert.match(approvalStore, /if \(externalApprovals\.approvals\.length > 0 \|\| externalApprovals\.directories\.length > 0 \|\| retryPending\) void refreshExternalApprovals\(\)/);
assert.match(approvalStore, /disabled = true;/);
assert.match(approvalActions, /'allow_once'/);
assert.doesNotMatch(approvalActions, /allow_for_turn/);
const toolApprovalAction = read('web/src/components/ToolApprovalAction.svelte');
assert.match(toolApprovalAction, /offersTurnWideAllow/);
assert.match(toolApprovalAction, /approval\.toolName !== 'agent_role_save'/);
assert.match(toolApprovalAction, /\{#if offersTurnWideAllow\}[\s\S]*?allow_for_turn/);
// 窗口不在前台时新的待确认发系统通知；启动时已存在的不重复提醒。
assert.match(approvalStore, /new Notification\(/);
assert.match(approvalStore, /document\.hasFocus\(\)/);
assert.match(approvalStore, /if \(!loadedOnce\)/);
assert.match(tray, /<ExternalApprovalActions \{approval\} \/>/);
// 外部会话是真正的会话：在主对话区打开，输入区换成只读提示，待确认走会话自己的授权托盘（同一个组件）。
const threadPanel = read('web/src/components/ThreadPanel.svelte');
assert.match(threadPanel, /isExternalSessionId\(messagesState\.currentSessionId\)[\s\S]*?<ExternalSessionBar \/>[\s\S]*?<InputArea \/>/);
assert.match(read('web/src/components/ExternalSessionBar.svelte'), /externalSession\.readOnlyNote/);
assert.doesNotMatch(read('web/src/components/ExternalSessionBar.svelte'), /textarea|<InputArea/);
// 设置页只留调用审计，待确认不再在设置里重复出现。
assert.doesNotMatch(section, /resolveMcpServerApproval|data-mcp-server-approvals/);

// ── i18n：用到的键中英文都存在，且两边键集合一致 ─────────────────────────────
const usedKeys = new Set();
for (const action of ['token.view_secret', 'token.update', 'token.rotate']) usedKeys.add(`mcpServer.audit.action.${action}`);
for (const id of ['clients', 'remote', 'activity']) usedKeys.add(`mcpServer.tab.${id}`);
for (const source of [section, tray, audit, approvalActions, approvalStore, read('web/src/components/ExternalSessionBar.svelte')]) {
  for (const match of source.matchAll(/i18n\.t\(\s*['`](mcpServer\.[^'`$]+)['`]/g)) usedKeys.add(match[1]);
}
for (const mode of ['ask', 'read_write', 'always']) {
  usedKeys.add(`mcpServer.token.approvalMode.${mode}`);
  usedKeys.add(`mcpServer.token.approvalModeShort.${mode}`);
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

// 执行实际加载函数，覆盖折叠状态和迟到响应，避免 OAuth 浏览时自动读取手动令牌。
const instance = parse(section).instance;
const script = section.slice(instance.content.start, instance.content.end);
const ast = ts.createSourceFile('SettingsMcpServerSection.ts', script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const functions = ['loadQuickAdd', 'openNamedEditor', 'closeNamedEditor'].map(name => {
  const node = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name);
  assert.ok(node, `missing production function ${name}`);
  return node.getText(ast);
}).join('\n');
const makeLoader = new Function('getMcpServerConfigSnippets', ts.transpileModule(`
  let tab = 'remote', showManualConfig = false, remoteReady = true;
  const token = { tokenId: 'client-1', hasSecret: true };
  let quickAddToken = token, quickAddTokenId = token.tokenId, quickAddSnippets = null;
  let quickAddRequest = 0;
  let status = { running: true, network: { namedHostname: 'mcp.example.com' } };
  let namedHostname = '', namedToken = '', showNamedEditor = false;
  ${functions}
  return {
    manual: loadQuickAdd,
    setOpen: value => { showManualConfig = value; }, setTab: value => { tab = value; },
    snapshot: () => ({ quickAddSnippets, namedHostname, namedToken, showNamedEditor }),
    openEditor: openNamedEditor, closeEditor: closeNamedEditor,
    typeToken: value => { namedToken = value; },
  };
`, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } }).outputText);
let requests = [];
const loader = makeLoader(tokenId => new Promise(resolve => { requests.push({ tokenId, resolve }); }));
await loader.manual();
assert.equal(requests.length, 0, '网络页未展开手动配置，不得请求令牌片段');
loader.setOpen(true);
const pending = loader.manual();
assert.equal(requests.length, 1);
loader.setOpen(false);
await loader.manual();
requests[0].resolve({ secretFilled: true });
await pending;
assert.equal(loader.snapshot().quickAddSnippets, null, '收起后迟到的令牌片段必须丢弃');
loader.setOpen(true);
const remotePending = loader.manual();
assert.equal(requests.length, 2, '展开手动连接配置后正常加载');
requests[1].resolve({ secretFilled: true });
await remotePending;
assert.equal(loader.snapshot().quickAddSnippets.secretFilled, true);
loader.setTab('clients');
await loader.manual();
assert.equal(loader.snapshot().quickAddSnippets, null, '离开网络分组应丢弃手动令牌片段');
loader.openEditor();
assert.equal(loader.snapshot().namedHostname, 'mcp.example.com', '修改时预填已配置域名');
loader.typeToken('test-only-secret');
loader.closeEditor();
assert.equal(loader.snapshot().namedToken, '', '取消编辑必须清空隧道令牌');
assert.equal(loader.snapshot().showNamedEditor, false);

console.log('mcp-server golden ok');
