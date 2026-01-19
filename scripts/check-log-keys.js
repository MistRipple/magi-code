#!/usr/bin/env node
/*
 * 轻量日志规范检查：要求 logger.* 第一个参数为中文分段 key。
 * 允许英文专有词：CLI/ACE/Webview/IDE/Lint/AI/LLM/Claude/Codex/Gemini。
 * 不做 AST 解析，仅做保守正则检测。
 */
const fs = require('fs');
const path = require('path');

const root = process.argv[2] || path.resolve(__dirname, '..', 'src');
const allowTokens = new Set(['CLI','ACE','Webview','IDE','Lint','AI','LLM','Claude','Codex','Gemini','claude','codex','gemini']);

function walk(dir, files = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'node_modules' || entry.name === 'out' || entry.name === 'dist') continue;
      walk(full, files);
    } else if (entry.isFile() && full.endsWith('.ts')) {
      files.push(full);
    }
  }
  return files;
}

const loggerPattern = /logger\.(info|warn|error|debug)\(\s*(['"])([^'"\n]+)\2/g;
const latinTokenPattern = /[A-Za-z]+/g;

let violations = [];
for (const file of walk(root)) {
  const text = fs.readFileSync(file, 'utf8');
  let match;
  while ((match = loggerPattern.exec(text)) !== null) {
    const key = match[3];
    if (!key.includes('.') && !key.includes('_')) continue;
    const tokens = key.match(latinTokenPattern) || [];
    const bad = tokens.filter(t => !allowTokens.has(t));
    if (bad.length > 0) {
      const line = text.slice(0, match.index).split('\n').length;
      violations.push({ file, line, key, bad });
    }
  }
}

if (violations.length) {
  console.error('日志 key 规范检查未通过：');
  for (const v of violations) {
    console.error(`- ${path.relative(process.cwd(), v.file)}:${v.line}  ${v.key}  (包含未允许的英文片段: ${v.bad.join(', ')})`);
  }
  process.exit(1);
}

console.log('日志 key 规范检查通过');
