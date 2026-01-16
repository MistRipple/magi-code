#!/usr/bin/env node
/*
 * Headless analysis probe for orchestrator planning.
 * Usage:
 *   node scripts/test-orchestrator-analyze.js --prompt "列出当前目录的文件结构"
 */

const path = require('path');
const fs = require('fs');

function parseArgs() {
  const args = process.argv.slice(2);
  const result = { prompt: '列出当前目录的文件结构', context: '' };
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === '--prompt' && args[i + 1]) {
      result.prompt = args[i + 1];
      i += 1;
    } else if (arg === '--context' && args[i + 1]) {
      result.context = args[i + 1];
      i += 1;
    }
  }
  return result;
}

function preview(text, max = 800) {
  const trimmed = (text || '').trim();
  if (!trimmed) return '';
  return trimmed.length > max ? `${trimmed.slice(0, max)}...` : trimmed;
}

function ensureDir(dirPath) {
  if (!fs.existsSync(dirPath)) {
    fs.mkdirSync(dirPath, { recursive: true });
  }
}

async function main() {
  const { prompt, context } = parseArgs();

  const { CLIAdapterFactory } = require('../out/cli/adapter-factory.js');
  const { buildOrchestratorAnalysisPrompt } = require('../out/orchestrator/prompts/orchestrator-prompts.js');

  const cliFactory = new CLIAdapterFactory({
    cwd: process.cwd(),
    timeout: 300000,
    idleTimeout: 120000,
    maxTimeout: 900000,
    cliPaths: {
      claude: 'claude',
      codex: 'codex',
      gemini: 'gemini'
    }
  });

  const analysisPrompt = buildOrchestratorAnalysisPrompt(
    prompt,
    ['claude', 'codex', 'gemini'],
    context || undefined
  );

  console.log('--- Orchestrator Analysis Probe ---');
  console.log('Prompt:', prompt);
  console.log('Context length:', (context || '').length);
  console.log('Analysis prompt length:', analysisPrompt.length);

  try {
    const response = await cliFactory.sendMessage(
      'claude',
      analysisPrompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: false,
        adapterRole: 'orchestrator',
        messageMeta: { intent: 'orchestrator_analyze_probe' }
      }
    );

    const content = response.content || '';
    const raw = response.raw || '';
    const outputDir = path.join(process.cwd(), '.tmp');
    ensureDir(outputDir);
    const stamp = new Date().toISOString().replace(/[:.]/g, '-');
    const rawPath = path.join(outputDir, `orchestrator-analyze-raw-${stamp}.log`);
    const contentPath = path.join(outputDir, `orchestrator-analyze-content-${stamp}.log`);
    fs.writeFileSync(rawPath, raw || '', 'utf-8');
    fs.writeFileSync(contentPath, content || '', 'utf-8');
    console.log('Response error:', response.error || 'none');
    console.log('Content length:', content.length);
    console.log('Raw length:', raw.length);
    console.log('Content preview:', preview(content));
    console.log('Raw preview:', preview(raw));
    console.log('Raw saved to:', rawPath);
    console.log('Content saved to:', contentPath);

    if (!content.trim()) {
      console.log('WARN: content is empty. Check raw output and CLI parameters.');
    }
  } catch (error) {
    console.error('Probe failed:', error && error.message ? error.message : error);
    process.exit(1);
  } finally {
    await cliFactory.dispose();
  }
}

main().catch(err => {
  console.error(err);
  process.exit(1);
});
