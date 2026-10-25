/** All sessions, results, durations and files below are fictional design fixtures. */
export const demoNotice = '交互设计预览 · 所有会话、工具结果与文件均为演示数据';

export type DemoStatus = 'success' | 'running' | 'error' | 'pending';
export type DemoCardKind = 'thought' | 'plan' | 'steps' | 'search' | 'terminal' | 'tool' | 'mcp' | 'diff' | 'code' | 'agents' | 'approval' | 'question' | 'diagram';
export interface DemoListItem { title: string; detail?: string; status?: DemoStatus; meta?: string; }
export interface DemoCard {
  id: string;
  kind: DemoCardKind;
  title: string;
  status?: DemoStatus;
  summary?: string;
  meta?: string;
  body?: string;
  code?: string;
  language?: string;
  items?: DemoListItem[];
  files?: string[];
  actions?: string[];
  expanded?: boolean;
}
export interface DemoMessage { id: string; role: 'user' | 'assistant'; author: string; time: string; text?: string; cards?: DemoCard[]; }
export interface DemoSession { id: string; title: string; group: '今天' | '昨天'; preview: string; time: string; status?: DemoStatus; }
export interface DemoPreviewFile { id: string; path: string; name: string; language: 'typescript' | 'markdown' | 'json'; content: string; added: number; removed: number; }

export const workbenchSessions: DemoSession[] = [
  { id: 'markdown-export', title: '为会话增加 Markdown 导出', group: '今天', preview: '导出已完成，正在同步使用说明', time: '14:32', status: 'running' },
  { id: 'mcp-library', title: '整理项目知识库', group: '今天', preview: '已归档 12 篇文档', time: '11:08', status: 'success' },
  { id: 'ui-review', title: '检查设置页的交互细节', group: '今天', preview: '统一键盘操作与聚焦样式', time: '09:46', status: 'success' },
  { id: 'release-notes', title: '整理本周更新', group: '昨天', preview: '更新说明已整理，可继续编辑', time: '18:24', status: 'success' },
  { id: 'api-investigation', title: '排查接口连接超时', group: '昨天', preview: '检查远端服务后可重试', time: '16:10', status: 'error' },
];

export const previewFiles: DemoPreviewFile[] = [
  { id: 'export-helper', path: 'src/lib/export-markdown.ts', name: 'export-markdown.ts', language: 'typescript', added: 24, removed: 0,
    content: `export interface ExportMessage {
  role: 'user' | 'assistant';
  content: string;
}

export function toMarkdown(title: string, messages: ExportMessage[]): string {
  const heading = title.replace(/[\\r\\n]+/g, ' ').trim();
  const sections = messages.map(({ role, content }) => {
    const author = role === 'user' ? '你' : 'Magi';
    return [\`## \${author}\`, content.trim()].join('\\n\\n');
  });
  return [\`# \${heading}\`, ...sections].join('\\n\\n') + '\\n';
}

export function downloadMarkdown(filename: string, content: string): void {
  const blob = new Blob([content], { type: 'text/markdown;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
` },
  { id: 'export-guide', path: 'docs/会话导出与离线阅读使用说明.md', name: '会话导出与离线阅读使用说明.md', language: 'markdown', added: 18, removed: 2,
    content: `# 导出会话

在会话右上角打开「更多」，选择「导出 Markdown」。
浏览器会保存一个 .md 文件，文件名来自当前会话标题。

## 保留的内容

- 你的消息与 Magi 的回答
- 代码块、列表、链接与表格
- 消息的原始顺序

## 不包含的内容

内部思考、工具执行日志与未发送的草稿不写入导出文件。
附件使用原始链接；在其他设备离线阅读时，附件可能无法访问。

## 数据范围

导出使用当前会话中已加载的消息，不会上传到额外的服务。
` },
  { id: 'export-sample', path: 'examples/markdown-export.json', name: 'markdown-export.json', language: 'json', added: 16, removed: 0,
    content: JSON.stringify({ title: '为会话增加 Markdown 导出', filename: '为会话增加 Markdown 导出.md', messages: [
      { role: 'user', content: '帮我整理导出方案。' },
      { role: 'assistant', content: '可以在会话菜单中增加「导出 Markdown」，并保留代码块与列表。' },
    ], options: { includeToolLogs: false, preserveMarkdown: true } }, null, 2) + '\n' },
];

export const workbenchConversation: DemoMessage[] = [
  { id: 'request', role: 'user', author: '你', time: '14:28', text: '帮这个项目增加「导出 Markdown」。放在会话的更多菜单里，保留正文和代码块，不导出工具日志。顺便补上使用说明。' },
  { id: 'investigation', role: 'assistant', author: 'Magi', time: '14:28', text: '我会先确认会话菜单和消息结构，再补上导出入口与说明。', cards: [
    { id: 'thought', kind: 'thought', title: '思考过程', summary: '沿用现有会话菜单与消息读取入口', meta: '3 秒', status: 'success', expanded: false, body: '导出只包含用户消息与最终回答，避免把内部过程混入文档。生成文件时保留原始 Markdown，代码块不再经过 HTML 转换。' },
    { id: 'plan', kind: 'plan', title: '实现计划', summary: '3 项工作', status: 'success', expanded: true, items: [
      { title: '确认菜单与消息数据入口', status: 'success' }, { title: '实现导出并补充回归检查', status: 'success' }, { title: '补全使用说明与导出示例', status: 'success' },
    ] },
    { id: 'search', kind: 'search', title: '搜索项目', summary: '会话菜单与 Markdown 内容', meta: '6 个匹配 · 0.3 秒', status: 'success', expanded: false, items: [
      { title: 'SessionMenu.svelte', detail: '会话操作菜单与已有的复制入口', meta: 'src/components' },
      { title: 'message.ts', detail: '用户消息与回答正文的类型定义', meta: 'src/types' },
      { title: 'MarkdownContent.svelte', detail: '代码块与正文的展示边界', meta: 'src/components' },
    ] },
    { id: 'agents', kind: 'agents', title: '代理协作', summary: '2 个代理完成检查', meta: '24 秒', status: 'success', expanded: true, items: [
      { title: '实现检查', detail: '沿用会话菜单，未引入新的持久化入口', status: 'success', meta: '14 秒' },
      { title: '导出验证', detail: '代码块、多段正文和空会话均已覆盖', status: 'success', meta: '24 秒' },
    ] },
  ] },
  { id: 'changes', role: 'assistant', author: 'Magi', time: '14:30', text: '导出入口已接入会话菜单。下面是核心变更和检查结果。', cards: [
    { id: 'steps', kind: 'steps', title: '执行步骤', summary: '3 / 3', status: 'success', expanded: false, items: [
      { title: '读取会话正文', status: 'success', meta: '0.4 秒' }, { title: '生成 Markdown 文件', status: 'success', meta: '1.2 秒' }, { title: '检查格式与导出结果', status: 'success', meta: '4.8 秒' },
    ] },
    { id: 'diff', kind: 'diff', title: '文件变更', summary: '3 个文件', meta: '+58 −2', status: 'success', expanded: true, files: previewFiles.map((file) => file.id), language: 'diff', code: `@@ 会话菜单
  { label: '复制会话', action: copyConversation },
+ { label: '导出 Markdown', action: exportMarkdown },

@@ 导出内容
+ const markdown = toMarkdown(session.title, visibleMessages);
+ downloadMarkdown(filename, markdown);`, actions: ['查看变更'] },
    { id: 'terminal', kind: 'terminal', title: '终端', summary: 'npm run check && npm run test:export', meta: '4.8 秒', status: 'success', expanded: true, language: 'text', code: `$ npm run check && npm run test:export

svelte-check found 0 errors and 0 warnings

 ✓ 保留 Markdown 代码块
 ✓ 按顺序导出用户消息与回答
 ✓ 跳过工具日志和空白内容
 ✓ 正确处理中文文件名

 Test Files  1 passed (1)
      Tests  4 passed (4)`, body: '以上为固定演示输出。' },
    { id: 'diagram', kind: 'diagram', title: '导出流程', summary: 'Markdown 产物', status: 'success', expanded: true, items: [
      { title: '会话正文', detail: '用户消息与最终回答' }, { title: 'Markdown', detail: '保留原始格式' }, { title: '本地文件', detail: '浏览器下载' },
    ], actions: ['预览说明'] },
  ] },
  { id: 'answer', role: 'assistant', author: 'Magi', time: '14:31', text: '已完成。打开会话右上角的「更多」，即可导出 Markdown。文件保留正文、代码块和原有消息顺序，工具日志不会写入。\n\n使用说明与示例也已补齐，可以在右侧预览。', cards: [
    { id: 'code', kind: 'code', title: '导出结果示例', summary: 'Markdown', status: 'success', expanded: true, language: 'markdown', code: '# 项目讨论\n\n## 你\n\n请帮我解释这段代码。\n\n## Magi\n\n可以使用下面的方式：\n\n```typescript\nconst result = toMarkdown(title, messages);\n```', actions: ['复制'] },
  ] },
  { id: 'followup', role: 'user', author: '你', time: '14:32', text: '再把使用说明同步到团队知识库，发布前让我确认。' },
  { id: 'sync', role: 'assistant', author: 'Magi', time: '14:32', text: '正在读取知识库目录。发布前会停下来等你确认。', cards: [
    { id: 'mcp', kind: 'mcp', title: '读取知识库目录', summary: 'MCP · 团队知识库', status: 'running', meta: '2.4 秒', expanded: false, body: '正在获取「产品使用说明」目录与已有文档标题。' },
    { id: 'failure', kind: 'tool', title: '读取发布模板', summary: '连接超时', status: 'error', meta: '10.0 秒', expanded: true, body: '模板服务暂时未响应。文档内容已保留，可以重试读取。', code: 'GET /templates/product-guide\nRequest timed out after 10s', language: 'text', actions: ['重试'] },
    { id: 'question', kind: 'question', title: '选择发布目录', summary: '需要你的选择', status: 'pending', expanded: true, body: '这份使用说明放在哪个目录？', items: [
      { title: '产品使用说明', detail: '与现有功能指南放在一起' }, { title: '团队内部文档', detail: '仅供项目成员参考' },
    ], actions: ['确认目录'] },
    { id: 'approval', kind: 'approval', title: '发布前确认', summary: '等待授权', status: 'pending', expanded: true, body: '将「会话导出与离线阅读使用说明」发布到选定目录。不会覆盖已有文档。', files: ['export-guide'], actions: ['允许本次发布', '暂不发布'] },
  ] },
];

export const workbenchRuntime = {
  sessionId: 'markdown-export', title: '同步使用说明', status: 'waiting' as const,
  summary: '等待选择发布目录与授权', elapsed: '02:46', model: '默认模型', contextUsedPercent: 18,
  completedTools: 9, runningTools: 1, failedTools: 1, changedFiles: previewFiles.length,
  agents: [
    { id: 'implementation-review', title: '实现检查', status: 'success' as DemoStatus, detail: '导出入口与消息边界' },
    { id: 'export-review', title: '导出验证', status: 'success' as DemoStatus, detail: '代码块与中文文件名' },
  ],
};
