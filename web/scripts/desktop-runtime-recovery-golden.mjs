import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

const appSource = fs.readFileSync(path.resolve('src/App.svelte'), 'utf8');
const componentSource = fs.readFileSync(path.resolve('src/components/DesktopRuntimeRecovery.svelte'), 'utf8');
const recoverySource = fs.readFileSync(path.resolve('src/lib/desktop-runtime-recovery.ts'), 'utf8');
const bootstrapSource = fs.readFileSync(path.resolve('../apps/desktop/bootstrap/index.html'), 'utf8');
const desktopSource = fs.readFileSync(path.resolve('../apps/desktop/src/main.rs'), 'utf8');
const nativeRecoverySource = fs.readFileSync(path.resolve('../apps/desktop/src/runtime_recovery.rs'), 'utf8');

assert.match(
  appSource,
  /runtimeRecoveryVisible[\s\S]*setTimeout\([\s\S]*3_000/,
  'brief connection interruptions must get an automatic recovery window before showing controls',
);
assert.match(appSource, /<DesktopRuntimeRecovery/, 'persistent desktop failures must be handled in the primary conversation area');
assert.match(recoverySource, /get_desktop_runtime_recovery/, 'the desktop UI must request an authoritative native diagnosis');
assert.match(recoverySource, /restart_desktop_runtime/, 'the desktop UI must use the native runtime supervisor to restart');
assert.doesNotMatch(
  recoverySource + componentSource,
  /\b(?:kill|taskkill|lsof|netstat|Get-NetTCPConnection)\b/i,
  'the web layer must never construct process termination commands',
);
assert.match(
  componentSource,
  /requiresConfirmation[\s\S]*confirmingExternal[\s\S]*runtimeConfirmStopAndRestart/,
  'non-Magi listeners must require a second explicit confirmation step',
);
assert.match(
  componentSource,
  /status === 'ready'[\s\S]*requestEnvironmentRecovery[\s\S]*runtimeRestoreEnvironment/,
  'a healthy endpoint with a stuck client must expose runtime environment recovery',
);
assert.doesNotMatch(
  componentSource + bootstrapSource,
  /runtimeReconnect|重新连接|重新打开工作台/,
  'persistent desktop recovery must rebuild the runtime environment instead of offering a connection-only action',
);
assert.match(componentSource, /processName[\s\S]*PID \{occupant\.pid\}[\s\S]*executablePath/, 'confirmation must show process identity details');
assert.match(componentSource, /technicalDetail[\s\S]*<details/, 'real native errors must remain inspectable');
assert.match(bootstrapSource, /window\.__TAURI__\?\.core\?\.invoke/, 'the packaged startup page must use the official Tauri global API');
assert.match(bootstrapSource, /get_desktop_runtime_recovery[\s\S]*restart_desktop_runtime/, 'startup failure recovery must use the same native commands');
assert.match(desktopSource, /restart_lock\.lock\(\)\.await/, 'runtime restarts must be serialized');
assert.match(
  desktopSource,
  /stop_current_daemon_for_recovery[\s\S]*wait_until_stopped[\s\S]*force_shutdown[\s\S]*强制中止并继续恢复运行环境/,
  'the desktop supervisor must force-stop a stuck managed daemon and continue rebuilding the runtime environment',
);
assert.match(
  desktopSource,
  /DESKTOP_ROUTE_QUERY_KEYS[\s\S]*workspaceId[\s\S]*workspacePath[\s\S]*sessionId[\s\S]*current_url/,
  'runtime recovery navigation must preserve the active workspace and session binding',
);
assert.match(
  nativeRecoverySource,
  /actual_identity != expected_identity[\s\S]*diagnose_port\(port, state_root\)[\s\S]*started_at[\s\S]*target\.start_time\(\) != expected\.started_at/,
  'native cleanup must revalidate port ownership and process start time before termination',
);
assert.match(nativeRecoverySource, /Signal::Term[\s\S]*target\.kill\(\)/, 'native cleanup must attempt graceful termination before a forced stop');

console.log('desktop runtime recovery golden replay passed');
