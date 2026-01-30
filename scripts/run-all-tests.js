#!/usr/bin/env node
/* eslint-disable no-console */
const { spawnSync } = require('child_process');
const path = require('path');

function run(cmd, args, cwd) {
  const result = spawnSync(cmd, args, { stdio: 'inherit', cwd });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

const mode = process.argv[2] || 'quick';
const repoRoot = path.join(__dirname, '..');

run('npm', ['run', 'compile'], repoRoot);

const runnerPath = path.join(repoRoot, 'out', 'test', 'run-all-tests.js');
run('node', [runnerPath, mode], repoRoot);
