const fs = require('fs');
const path = require('path');

const checks = [
  {
    name: 'MessageDeduplicator shouldSendUpdate exists',
    file: 'src/normalizer/message-deduplicator.ts',
    pattern: /shouldSendUpdate\(/,
  },
  {
    name: 'WebviewProvider uses shouldSendUpdate for standardUpdate',
    file: 'src/ui/webview-provider.ts',
    pattern: /shouldSendUpdate\(update\)/,
  },
  {
    name: 'Snapshot uses subTask.id when creating snapshots',
    file: 'src/orchestrator/orchestrator-agent.ts',
    pattern: /info\.subTaskId/,
  },
  {
    name: 'Snapshot cleanup uses getSnapshotFilePath',
    file: 'src/session/unified-session-manager.ts',
    pattern: /getSnapshotFilePath/,
  },
  {
    name: 'Dependency graph normalizes targetFiles',
    file: 'src/orchestrator/worker-pool.ts',
    pattern: /resolveTargetFilesForGraph\(subTask\)/,
  },
  {
    name: 'CLI output buffer cleared on response',
    file: 'src/ui/webview-provider.ts',
    pattern: /this\.cliOutputs\.set\(type, \[\]\);/,
  },
];

let failed = 0;
for (const check of checks) {
  const filePath = path.join(process.cwd(), check.file);
  const content = fs.readFileSync(filePath, 'utf8');
  const ok = check.pattern.test(content);
  if (ok) {
    console.log(`✅ ${check.name}`);
  } else {
    console.error(`❌ ${check.name}`);
    failed += 1;
  }
}

if (failed > 0) {
  console.error(`\n${failed} check(s) failed.`);
  process.exit(1);
}

console.log('\nManual sanity checks passed.');
