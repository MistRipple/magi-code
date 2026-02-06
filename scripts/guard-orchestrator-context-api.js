#!/usr/bin/env node
/* eslint-disable no-console */

const fs = require('fs');
const path = require('path');

const ROOT = process.cwd();
const TARGET_DIRS = [
  path.join(ROOT, 'src', 'orchestrator', 'core'),
];
const TARGET_FILES = [
  path.join(ROOT, 'src', 'orchestrator', 'worker', 'autonomous-worker.ts'),
];

const BANNED_PATTERNS = [
  { name: 'getContext', regex: /\.\s*getContext\s*\(/ },
  { name: 'getContextSlice', regex: /\.\s*getContextSlice\s*\(/ },
];

function collectTsFilesFromDir(dirPath) {
  if (!fs.existsSync(dirPath)) return [];
  const entries = fs.readdirSync(dirPath, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const fullPath = path.join(dirPath, entry.name);
    if (entry.isDirectory()) {
      files.push(...collectTsFilesFromDir(fullPath));
      continue;
    }
    if (entry.isFile() && fullPath.endsWith('.ts')) {
      files.push(fullPath);
    }
  }
  return files;
}

function collectTargets() {
  const dirFiles = TARGET_DIRS.flatMap(collectTsFilesFromDir);
  const explicitFiles = TARGET_FILES.filter((filePath) => fs.existsSync(filePath));
  return Array.from(new Set([...dirFiles, ...explicitFiles]));
}

function scanFile(filePath) {
  const content = fs.readFileSync(filePath, 'utf8');
  const lines = content.split('\n');
  const violations = [];

  lines.forEach((line, index) => {
    BANNED_PATTERNS.forEach((pattern) => {
      if (pattern.regex.test(line)) {
        violations.push({
          filePath,
          line: index + 1,
          api: pattern.name,
          content: line.trim(),
        });
      }
    });
  });

  return violations;
}

function main() {
  const targets = collectTargets();
  if (targets.length === 0) {
    console.log('[guard-orchestrator-context-api] 未发现目标文件，跳过。');
    return;
  }

  const allViolations = targets.flatMap(scanFile);
  if (allViolations.length === 0) {
    console.log('[guard-orchestrator-context-api] 通过：主编排链路未发现 getContext/getContextSlice 调用。');
    return;
  }

  console.error('[guard-orchestrator-context-api] 失败：检测到主编排链路调用旧上下文接口。');
  allViolations.forEach((v) => {
    const relative = path.relative(ROOT, v.filePath);
    console.error(`- ${relative}:${v.line} 使用 ${v.api} -> ${v.content}`);
  });
  process.exit(1);
}

main();
