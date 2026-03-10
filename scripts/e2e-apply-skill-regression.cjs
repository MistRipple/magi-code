#!/usr/bin/env node
/**
 * apply_skill 端到端回归脚本
 *
 * 验证：
 * 1. apply_skill 工具注册到 getBuiltinTools()
 * 2. buildToolsSummary() 在有 Instruction Skill 时输出目录段
 * 3. buildToolsSummary() 在无 Instruction Skill 时不输出目录段
 * 4. execute('apply_skill') 正确返回 Skill 指令内容
 * 5. execute('apply_skill') Skill 不存在时返回错误及可用列表
 * 6. execute('apply_skill') 缺少参数时返回错误
 * 7. 别名 'apply-skill' 正确映射到 'apply_skill'
 */

const path = require('path');
const fs = require('fs');
const Module = require('module');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

// ============================================================================
// vscode stub（与其他 e2e 脚本一致）
// ============================================================================
function installVscodeStub() {
  const originalLoad = Module._load;
  Module._load = function patchedLoad(request, parent, isMain) {
    if (request === 'vscode') {
      return {
        workspace: {
          workspaceFolders: [],
          getConfiguration: () => ({ get: () => undefined }),
          fs: { stat: async () => ({}), readDirectory: async () => [], readFile: async () => Buffer.from('') },
          findFiles: async () => [],
          openTextDocument: async () => ({
            uri: { fsPath: '', toString: () => '' }, getText: () => '',
            positionAt: () => ({ line: 0, character: 0 }),
            lineAt: () => ({ text: '' }), languageId: 'typescript',
          }),
        },
        window: {
          createOutputChannel: () => ({ appendLine() {}, append() {}, clear() {}, show() {}, dispose() {} }),
          showErrorMessage: async () => undefined, showWarningMessage: async () => undefined,
          showInformationMessage: async () => undefined,
          onDidCloseTerminal: () => ({ dispose() {} }), onDidOpenTerminal: () => ({ dispose() {} }),
          createTerminal: () => ({ sendText() {}, show() {}, dispose() {} }),
          terminals: [], activeTextEditor: undefined, visibleTextEditors: [],
        },
        commands: { executeCommand: async () => undefined, registerCommand: () => ({ dispose() {} }) },
        languages: { getDiagnostics: () => [] },
        env: { shell: process.env.SHELL || '/bin/zsh', clipboard: { readText: async () => '', writeText: async () => {} } },
        Uri: {
          file: (p) => ({ fsPath: p, path: p, toString: () => p }),
          parse: (p) => ({ fsPath: p, path: p, toString: () => p }),
          joinPath: (...parts) => ({ fsPath: parts.map(p => typeof p === 'string' ? p : p.path || '').join('/'), toString() { return this.fsPath; } }),
        },
        EventEmitter: class { constructor() { this.listeners = new Set(); this.event = (l) => { this.listeners.add(l); return { dispose: () => this.listeners.delete(l) }; }; } fire(d) { for (const l of this.listeners) { try { l(d); } catch {} } } dispose() { this.listeners.clear(); } },
        Disposable: class { dispose() {} },
        Position: class { constructor(l, c) { this.line = l; this.character = c; } },
        Range: class { constructor(s, e) { this.start = s; this.end = e; } },
        Selection: class { constructor(a, b) { this.anchor = a; this.active = b; } },
        RelativePattern: class { constructor(b, p) { this.baseUri = b; this.pattern = p; } },
        ViewColumn: { One: 1, Two: 2, Three: 3 },
      };
    }
    return originalLoad.call(this, request, parent, isMain);
  };
}

// ============================================================================
// 测试主体
// ============================================================================
const results = [];
function assert(name, condition, detail) {
  results.push({ name, pass: !!condition, detail: detail || '' });
  console.log(`  ${condition ? '✅' : '❌'} ${name}${detail ? ' — ' + detail : ''}`);
}

async function main() {
  if (!fs.existsSync(path.join(OUT, 'tools', 'tool-manager.js'))) {
    throw new Error('缺少 out 编译产物，请先执行 npm run compile');
  }

  installVscodeStub();

  const { ToolManager } = require(path.join(OUT, 'tools', 'tool-manager.js'));
  const { SkillsManager } = require(path.join(OUT, 'tools', 'skills-manager.js'));

  const toolManager = new ToolManager({
    workspaceRoot: ROOT,
    workspaceFolders: [{ name: 'magi', path: ROOT }],
    permissions: { allowEdit: false, allowBash: false, allowWeb: false },
  });

  // 准备 mock Instruction Skill 数据
  const mockSkills = [
    { name: 'code-review', description: '结构化代码审查', content: '# Code Review\n\n请按照以下清单审查代码:\n1. 安全性\n2. 性能\n3. 可维护性', disableModelInvocation: false, userInvocable: true },
    { name: 'deploy-guide', description: '部署流程指南', content: '# 部署步骤\n\n1. 构建\n2. 测试\n3. 发布', disableModelInvocation: true, userInvocable: true },
  ];
  const skillsManager = new SkillsManager({ customTools: [], instructionSkills: mockSkills });
  toolManager.registerSkillExecutor(skillsManager);

  console.log('\n=== apply_skill 端到端回归测试 ===\n');

  // ── Test 1: apply_skill 出现在 getBuiltinTools ──
  const allTools = await toolManager.getTools();
  const applySkillTool = allTools.find(t => t.name === 'apply_skill');
  assert('T1: apply_skill 工具已注册', !!applySkillTool);
  assert('T1.1: input_schema 包含 skill_name', applySkillTool?.input_schema?.properties?.skill_name?.type === 'string');
  assert('T1.2: skill_name 是必需参数', applySkillTool?.input_schema?.required?.includes('skill_name'));

  // ── Test 2: buildToolsSummary 包含 Instruction Skill 目录 ──
  const summary = await toolManager.buildToolsSummary({ role: 'orchestrator' });
  assert('T2: buildToolsSummary 包含 Instruction Skill 目录段', summary.includes('Installed Skill Instructions'));
  assert('T2.1: 目录包含 [auto] code-review', summary.includes('[auto] code-review'));
  assert('T2.2: 目录包含 [manual] deploy-guide', summary.includes('[manual] deploy-guide'));
  assert('T2.3: 目录包含 apply_skill 引导', summary.includes('apply_skill(skill_name)'));

  // ── Test 3: 无 Skill 时不显示目录 ──
  toolManager.unregisterSkillExecutor();
  const summaryNoSkill = await toolManager.buildToolsSummary({ role: 'orchestrator' });
  assert('T3: 无 Skill 时不包含 Instruction Skill 目录', !summaryNoSkill.includes('Installed Skill Instructions'));
  toolManager.registerSkillExecutor(skillsManager); // 恢复

  // ── Test 4: 正常执行 apply_skill ──
  const r4 = await toolManager.execute({ id: 't4', name: 'apply_skill', arguments: { skill_name: 'code-review' } });
  assert('T4: apply_skill 成功返回', !r4.isError);
  assert('T4.1: 返回内容包含 Skill 标题', String(r4.content).includes('Skill Instructions: code-review'));
  assert('T4.2: 返回内容包含指令正文', String(r4.content).includes('安全性'));

  // ── Test 5: Skill 不存在时返回错误 ──
  const r5 = await toolManager.execute({ id: 't5', name: 'apply_skill', arguments: { skill_name: 'nonexistent' } });
  assert('T5: 不存在的 Skill 返回错误', !!r5.isError);
  assert('T5.1: 错误信息列出可用 Skill', String(r5.content).includes('code-review') && String(r5.content).includes('deploy-guide'));

  // ── Test 6: 缺少参数时返回错误 ──
  const r6 = await toolManager.execute({ id: 't6', name: 'apply_skill', arguments: {} });
  assert('T6: 缺少 skill_name 返回错误', !!r6.isError);
  assert('T6.1: 错误信息提示缺少参数', String(r6.content).includes('skill_name'));

  // ── Test 7: 别名映射 ──
  const r7 = await toolManager.execute({ id: 't7', name: 'apply-skill', arguments: { skill_name: 'code-review' } });
  assert('T7: 别名 apply-skill 正确映射', !r7.isError);
  assert('T7.1: 别名执行结果与原名一致', String(r7.content).includes('Skill Instructions: code-review'));

  // ── 汇总 ──
  const total = results.length;
  const passed = results.filter(r => r.pass).length;
  const failed = results.filter(r => !r.pass);
  console.log(`\n=== 结果: ${passed}/${total} 通过 ===`);
  if (failed.length > 0) {
    console.log('失败项:');
    for (const f of failed) console.log(`  ❌ ${f.name}`);
  }
  process.exit(failed.length === 0 ? 0 : 2);
}

main().catch(error => {
  console.error('apply_skill 回归失败:', error?.stack || error);
  process.exitCode = 1;
});

