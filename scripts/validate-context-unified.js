#!/usr/bin/env node

/**
 * 统一上下文主链路静态校验
 *
 * 校验目标：
 * 1. Direct/Todo 路径上下文注入一致
 * 2. 目标文件读取走缓存前置逻辑
 * 3. 成功/失败都写入共享洞察
 * 4. 质量门禁包含核心检查项
 * 5. ContextManager 按 agent 维度提供最近轮次
 */

const fs = require('fs');
const path = require('path');

const root = process.cwd();
const workerFile = path.join(root, 'src/orchestrator/worker/autonomous-worker.ts');
const contextManagerFile = path.join(root, 'src/context/context-manager.ts');

function readFile(filePath) {
  if (!fs.existsSync(filePath)) {
    throw new Error(`文件不存在: ${filePath}`);
  }
  return fs.readFileSync(filePath, 'utf8');
}

function assertRule(content, regex, description) {
  if (!regex.test(content)) {
    throw new Error(`未通过: ${description}`);
  }
}

function run() {
  const workerContent = readFile(workerFile);
  const contextContent = readFile(contextManagerFile);

  const rules = [
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /executeDirectly\(assignment,\s*options,\s*sharedContext\)/,
      description: 'Direct 路径必须传入 sharedContext',
    },
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /buildExecutionPrompt\(\s*todo,\s*assignment,\s*options\.projectContext,\s*sharedContext,\s*targetFileContext/s,
      description: 'Todo 路径必须注入 sharedContext + targetFileContext',
    },
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /await this\.writeInsight\(this\.buildSuccessInsight\(/,
      description: '成功路径必须写入 insight',
    },
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /await this\.writeInsight\(this\.buildFailureInsight\(/,
      description: '失败路径必须写入 insight',
    },
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /const fileResult = await this\.readFileWithCache\(absolutePath\)/,
      description: '目标文件上下文必须走 readFileWithCache',
    },
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /质量门禁失败: 未注入共享上下文/,
      description: '质量门禁必须包含共享上下文检查',
    },
    {
      file: 'autonomous-worker.ts',
      content: workerContent,
      regex: /质量门禁失败: 目标文件未经过缓存前置读取/,
      description: '质量门禁必须包含缓存前置检查',
    },
    {
      file: 'context-manager.ts',
      content: contextContent,
      regex: /private async getRecentTurnsForAssembler\(\s*agentId:/,
      description: 'getRecentTurnsForAssembler 必须使用 agentId 参数',
    },
    {
      file: 'context-manager.ts',
      content: contextContent,
      regex: /private getAgentScopedTurnsFromSession\(/,
      description: 'ContextManager 必须实现 agent 级最近轮次提取',
    },
    {
      file: 'context-manager.ts',
      content: contextContent,
      regex: /shouldIncludeSessionMessageForAgent\(/,
      description: 'ContextManager 必须实现 agent 消息筛选规则',
    },
  ];

  for (const rule of rules) {
    assertRule(rule.content, rule.regex, `${rule.file}: ${rule.description}`);
  }

  console.log('统一上下文静态校验通过');
}

try {
  run();
} catch (error) {
  console.error('[validate-context-unified] 失败:', error.message);
  process.exit(1);
}
