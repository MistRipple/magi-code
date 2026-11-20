import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { cp, mkdir, mkdtemp, readdir, readFile, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { connectAppPage, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { installFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const appRoot = resolve(process.env.MAGI_PLUGIN_TEST_APP ?? 'target/electron-dist/mac-arm64/Magi.app');
const output = resolve(process.env.MAGI_DSH_HISTORY_OUTPUT ?? 'target/plugin-acceptance/phase-2-live-input');
await mkdir(output, { recursive: true });
const resources = join(appRoot, 'Contents/Resources');
const run = await mkdtemp(join(tmpdir(), 'magi-phase2-input-'));
const dshHome = join(run, 'plugins/dsh-home');
await mkdir(dshHome, { recursive: true, mode: 0o700 });
await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'));
const phase1 = JSON.parse(await readFile('docs/plugin-validation/phase-1/manifest.json'));
const phase2 = JSON.parse(await readFile('docs/plugin-validation/phase-2/manifest.json'));
const history = phase2.packages.find(sample => sample.name === 'dsh-plugin-message-history');
const buddy = phase1.packages.find(sample => sample.name === 'dsh-connect-workbuddy');
const digest = async path => createHash('sha256').update(await readFile(path)).digest('hex');

const packagedManifest = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
const result = {
  platform: 'macOS arm64', electron: packagedManifest.electronVersion,
  daemonSha256: await digest(join(resources, 'daemon/magi-daemon-app')),
  sdkLockSha256: await digest(join(resources, 'daemon/dsh/package-lock.json')),
  runtimeEntrySha256: await digest(join(resources, 'daemon/dsh/runtime/entry.mjs')),
  observerSha256: await digest(join(resources, 'daemon/dsh/runtime/session-observer.mjs')),
  samples: [history, buddy].map(({name, version, sha256, integrity}) => ({ name, version, sha256, integrity })),
  checks: {},
};
assert.equal(result.daemonSha256, packagedManifest.components.daemon.sha256);
const port = Number(process.env.MAGI_DSH_HISTORY_PORT ?? 38958);
const cdpPort = Number(process.env.MAGI_DSH_HISTORY_CDP_PORT ?? 9332);
const origin = `http://127.0.0.1:${port}`;
const pluginUrl = '/api/plugins/' + encodeURIComponent(history.name);
let app, page;
async function start() {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, 'isolated port');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: run, cdpPort, env: {
    // The original sample chooses its own storage. Isolate only this
    // acceptance process from the user's real DSH installation.
    DSH_HOME: dshHome, MAGI_STATE_ROOT: run, MAGI_HOST: '127.0.0.1',
    MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '',
    MAGI_DSH_SDK_PROJECT: '', MAGI_DESKTOP_REUSE_DAEMON: '',
    MAGI_NPM_REGISTRY: 'https://registry.npmjs.org',
  } });
  page = await waitFor(() => connectAppPage(cdpPort, port), 'isolated Chromium renderer', 60_000);
  await waitFor(() => pageTools(page).exists('[data-testid="sidebar-settings"]'), 'settings entry');
}
async function stop() {
  page?.close();
  page = undefined;
  if (app) await app.stop();
  app = undefined;
}
async function transport(path, body) {
  const response = await fetch(origin + path, {
    ...(body === undefined ? {} : { method: 'POST', body: JSON.stringify(body) }),
    headers: { origin, 'content-type': 'application/json' },
    signal: AbortSignal.timeout(120_000),
  });
  const raw = await response.text();
  return {status: response.status, body: raw ? JSON.parse(raw) : null};
}
async function request(path, body) {
  const response = await transport(path, body);
  assert.equal(response.status, 200, path + ' ' + JSON.stringify(response.body));
  return response.body;
}
async function inspect(sample) {
  const preview = await request('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: sample.name, version: sample.version });
  assert.equal(preview.candidate.digest, sample.sha256);
  assert.equal(preview.integrity, sample.integrity);
}
const installed = async () => (await request('/api/plugins')).plugins.find(row => row.manifest.id === history.name);
async function readHistory(instance, sessionId) {
  const response = await request(pluginUrl + '/routes', {
    method: 'GET', path: '/api/message-history-v2', query: 'sessionId=' + encodeURIComponent(sessionId),
    expectedDigest: instance.digest, expectedInstanceId: instance.runtime.instanceId,
  });
  assert.equal(response.status, 200);
  return response.body.history;
}
const submit = (sessionId, text) => request('/api/session/turn', {
  scope: 'personal', sessionId, text, requestId: randomUUID(), userMessageId: randomUUID(),
});
const awaitTurn = (sessionId, turnId) => waitFor(async () => {
  const { canonicalTurns } = await request('/api/messages?scope=personal&sessionId=' + encodeURIComponent(sessionId));
  return canonicalTurns?.find(turn => turn.turnId === turnId &&
    ['completed', 'failed', 'cancelled', 'blocked'].includes(turn.status));
}, 'real model turn settled', 180_000);
const text = 'Reply with only MAGI_HISTORY_PHASE_TWO.';
let failure;
try {
  await start();
  await inspect(history);
  await installFixedUpstreamFromNative({page, request, packageName: history.name, version: history.version});
  await inspect(buddy);
  await installFixedUpstreamFromNative({page, request, packageName: buddy.name, version: buddy.version});
  const model = await waitForPluginModel({request, pluginId: buddy.name, record: async () => {} });
  const { sessionId } = await request('/api/session/materialize', {scope: 'personal'});
  await request('/api/settings/orchestrator/session/save', {scope:'personal', sessionId,
    config: {sourceId: model.sourceId, model: model.id, reasoningEffort: null}});
  const before = await installed();
  assert.deepEqual(await readHistory(before, sessionId), []);
  const first = await submit(sessionId, text);
  await waitFor(async () => (await readHistory(before, sessionId)).includes(text),
    'original plugin records real accepted user input', 20000);
  const saved = JSON.parse(await readFile(join(dshHome, 'message-history.json')));
  assert.equal(saved.find(entry => entry.text === text)?.sessionId, sessionId);
  const eventRoot = join(run, 'session-events');
  const log = await readdir(eventRoot, {recursive: true});
  const inputTransaction = (await Promise.all(log.filter(path => path.endsWith('.json')).map(async path =>
    JSON.parse(await readFile(join(eventRoot, path)))))).find(transaction =>
    transaction.events?.some(event => event.item?.content === text && event.sessionId === sessionId) &&
    transaction.events.some(event => event.input?.state === 'attached' && event.input.attachedTurnId === first.turnId));
  assert.ok(inputTransaction);
  const eventSeq = inputTransaction.events.find(event => event.item?.content === text).eventSeq;
  const canonical = await awaitTurn(sessionId, first.turnId);
  assert.equal(canonical.status, 'completed');
  assert.equal(canonical.items.filter(item => item.kind === 'assistant_text').map(item => item.content).join('').trim(),
    'MAGI_HISTORY_PHASE_TWO');
  result.checks.liveOriginalRoute = { matchedSession: true, model: model.id, turnStatus: canonical.status, eventSeq };
  console.log('Original history recorded the completed real session.');

  await request(pluginUrl + '/disable', {expectedDigest: before.digest});
  const stale = await transport(pluginUrl + '/routes', { method:'GET', path:'/api/message-history-v2',
    expectedDigest: before.digest, expectedInstanceId: before.runtime.instanceId });
  assert.equal(stale.status, 403);
  const whileDisabled = await submit(sessionId, 'Reply with only MAGI_HISTORY_DISABLED.');
  assert.equal((await awaitTurn(sessionId, whileDisabled.turnId)).status, 'completed');
  await request(pluginUrl + '/enable', {expectedDigest: before.digest});
  const enabled = await waitFor(async () => {
    const value = await installed();
    return value?.runtime.state === 'running' && value;
  }, 'original history reenabled');
  assert.notEqual(enabled.runtime.instanceId, before.runtime.instanceId);
  assert.deepEqual(await readHistory(enabled, sessionId), [text]);
  const obsolete = await transport(pluginUrl + '/routes', { method:'GET', path:'/api/message-history-v2',
    expectedDigest: before.digest, expectedInstanceId: before.runtime.instanceId });
  assert.equal(obsolete.status, 409);
  result.checks.disabledInputNotObserved = { disabledRouteStatus: stale.status, replacedInstanceStatus: obsolete.status, presentMessages: 1 };
  console.log('Disabled plugin missed the new input and the old page was rejected.');

  const prepare = JSON.parse(await readFile(join(run, 'plugins/state.json'))).loading.configuration.preparation;
  const lock = join(prepare.projectDirectory, 'package-lock.json');
  result.dependencyLockSha256 = await digest(lock);
  await cp(lock, join(output, 'package-lock.json'));
  await stop();
  await start();
  const restored = await waitFor(async () => {
    const value = await installed();
    return value?.runtime.state === 'running' && value;
  }, 'original history restarted');
  assert.notEqual(restored.runtime.instanceId, enabled.runtime.instanceId);
  assert.deepEqual(await readHistory(restored, sessionId), [text]);
  const afterRestartText = 'Reply with only MAGI_HISTORY_RESTARTED.';
  const afterRestart = await submit(sessionId, afterRestartText);
  await waitFor(async () => (await readHistory(restored, sessionId)).includes(afterRestartText), 'new accepted input after restart');
  assert.equal((await awaitTurn(sessionId, afterRestart.turnId)).status, 'completed');
  assert.deepEqual(await readHistory(restored, sessionId), [afterRestartText, text]);
  result.checks.restartReadsOriginalStorage = { retainedInput: true, newInput: true, presentMessages: 2 };
  console.log('Full restart retained the original history and recorded the next live input.');
} catch (error) { failure = error; result.failure = error.message; }
finally {
  await stop();
  await writeFile(join(output, 'result.json'), JSON.stringify(result, null, 2) + '\n');
}
if (failure) throw failure;
