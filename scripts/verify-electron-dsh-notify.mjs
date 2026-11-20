// Verify the original Notify business buffer against committed Magi conversation outcomes.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { connectAppPage, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { installFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const appRoot = resolve(process.env.MAGI_PLUGIN_TEST_APP ?? 'target/electron-dist/mac-arm64/Magi.app');
const resources = join(appRoot, 'Contents/Resources');
const output = resolve(process.env.MAGI_DSH_NOTIFY_OUTPUT ?? 'target/plugin-acceptance/phase-2-notify');
const port = Number(process.env.MAGI_DSH_NOTIFY_PORT ?? 38961);
const cdpPort = Number(process.env.MAGI_DSH_NOTIFY_CDP_PORT ?? 9335);
const origin = `http://127.0.0.1:${port}`;
const state = await mkdtemp(join(tmpdir(), 'magi-phase2-notify-turns-'));
const credentialDir = join(state, 'plugins/dsh-home');
await mkdir(output, { recursive: true });
await mkdir(credentialDir, { recursive: true, mode: 0o700 });
await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(credentialDir, '.workbuddy-device.json'));
const phase1 = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-1/manifest.json', import.meta.url)));
const phase2 = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-2/manifest.json', import.meta.url)));
const notify = phase2.packages.find(sample => sample.name === '@idoall/dsh-notify');
const buddy = phase1.packages.find(sample => sample.name === 'dsh-connect-workbuddy');
const packaged = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const digest = async path => sha256(await readFile(path));
const result = {
  platform: `${process.platform} ${process.arch}`, electron: packaged.electronVersion,
  daemonSha256: await digest(join(resources, 'daemon/magi-daemon-app')),
  sdkLockSha256: await digest(join(resources, 'daemon/dsh/package-lock.json')),
  observerSha256: await digest(join(resources, 'daemon/dsh/runtime/session-observer.mjs')),
  samples: [notify, buddy].map(({ name, version, sha256, integrity }) => ({ name, version, sha256, integrity })),
  checks: {},
};
assert.equal(result.daemonSha256, packaged.components.daemon.sha256);

const sourceId = 'acceptance-notify-failure-source';
const deniedModel = 'acceptance-auth-denied';
const heldModel = 'acceptance-await-cancel';
let failedRequests = 0;
let heldRequests = 0;
const provider = createServer(async (request, response) => {
  if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
    response.writeHead(404).end();
    return;
  }
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const { model } = JSON.parse(Buffer.concat(chunks));
  if (model === heldModel) {
    heldRequests++;
    return;
  }
  assert.equal(model, deniedModel);
  failedRequests++;
  response.writeHead(401, { 'content-type': 'application/json' })
    .end(JSON.stringify({ error: { type: 'authentication_error', message: 'Notify acceptance provider denied authentication' } }));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
let app, page;
async function request(path, body) {
  const response = await fetch(origin + path, {
    ...(body === undefined ? {} : { method: 'POST', body: JSON.stringify(body) }),
    headers: { origin, 'content-type': 'application/json' },
    signal: AbortSignal.timeout(120_000),
  });
  const raw = await response.text();
  assert.equal(response.status, 200, `${path} ${response.status}: ${raw}`);
  return raw ? JSON.parse(raw) : null;
}
async function install(sample) {
  const preview = await request('/api/plugins/upstream/preview', {
    ecosystem: 'dsh', package: sample.name, version: sample.version,
  });
  assert.equal(preview.candidate.digest, sample.sha256);
  assert.equal(preview.integrity, sample.integrity);
  return installFixedUpstreamFromNative({ page, request, packageName: sample.name, version: sample.version });
}
async function submit(sessionId, text, model) {
  await request('/api/settings/orchestrator/session/save', {
    scope: 'personal', sessionId, config: { sourceId: model.sourceId, model: model.id, reasoningEffort: null },
  });
  return request('/api/session/turn', { scope: 'personal', sessionId, text, requestId: randomUUID(), userMessageId: randomUUID() });
}
const turnById = async (sessionId, turnId) => (await request('/api/messages?scope=personal&sessionId=' + encodeURIComponent(sessionId)))
  .canonicalTurns?.find(turn => turn.turnId === turnId);
const waitTurn = (sessionId, turnId) => waitFor(async () => {
  const turn = await turnById(sessionId, turnId);
  return ['completed', 'failed', 'cancelled', 'blocked'].includes(turn?.status) && turn;
}, 'committed turn settlement', 150_000);
async function pull(instance) {
  const route = await request('/api/plugins/' + encodeURIComponent(notify.name) + '/routes', {
    method: 'GET', path: '/plugins/dsh-notify/pull', expectedDigest: instance.digest,
    expectedInstanceId: instance.runtime.instanceId,
  });
  assert.equal(route.status, 200);
  return route.body;
}
let failure;
try {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, 'isolated port');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: state, cdpPort, env: {
    MAGI_STATE_ROOT: state, MAGI_HOST: '127.0.0.1', MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0',
    MAGI_DESKTOP_REUSE_DAEMON: '', MAGI_EXTENSION_NODE: '', MAGI_DSH_SDK_PROJECT: '',
    MAGI_NPM_REGISTRY: 'https://registry.npmjs.org',
  } });
  page = await waitFor(() => connectAppPage(cdpPort, port), 'isolated renderer', 60_000);
  await waitFor(() => pageTools(page).exists('[data-testid="sidebar-settings"]'), 'Magi settings');
  await request('/api/settings/orchestrator/save', { expectedRevision: 0, sources: [{
    id: sourceId, name: 'Notify acceptance transport',
    baseUrl: `http://127.0.0.1:${provider.address().port}/v1`,
    apiKey: 'isolated-test-placeholder', apiProtocol: 'openai_chat', models: [{ id: deniedModel }, { id: heldModel }],
  }] });
  await install(notify);
  await install(buddy);
  const model = await waitForPluginModel({ request, pluginId: buddy.name, record: async () => {} });
  const instance = (await request('/api/plugins')).plugins.find(plugin => plugin.manifest.id === notify.name);
  assert.equal(instance.runtime.state, 'running');
  assert.deepEqual((await pull(instance)).items, []);
  const { sessionId } = await request('/api/session/materialize', { scope: 'personal' });

  const completed = await submit(sessionId, 'Reply with exactly MAGI_NOTIFY_COMMITTED_OK.', model);
  const turn = await waitTurn(sessionId, completed.turnId);
  assert.equal(turn.status, 'completed');
  assert.equal(turn.items.filter(item => item.kind === 'assistant_text').map(item => item.content).join('').trim(),
    'MAGI_NOTIFY_COMMITTED_OK');
  const successNotice = await waitFor(async () => (await pull(instance)).items
    .find(item => item.sessionId === sessionId && item.kind === 'completed'), 'original completion notification', 25_000);
  assert.equal(successNotice.turn, 1);
  result.checks.completed = { provider: model.id, canonicalStatus: turn.status, notificationKind: successNotice.kind, turn: 1 };
  console.log('Notify original package received the real model completion.');

  const cancelled = await submit(sessionId, 'Wait for cancellation.', { sourceId: `custom/${sourceId}`, id: heldModel });
  await waitFor(() => heldRequests > 0, 'in-flight provider request');
  await request('/api/session/interrupt', { sessionId });
  assert.equal((await waitTurn(sessionId, cancelled.turnId)).status, 'cancelled');
  result.checks.cancelled = { canonicalStatus: 'cancelled', inFlight: true };

  const denied = await submit(sessionId, 'Trigger the isolated provider authentication error.', { sourceId: `custom/${sourceId}`, id: deniedModel });
  const failed = await waitTurn(sessionId, denied.turnId);
  assert.equal(failed.status, 'failed');
  const diagnostic = failed.items.find(item => item.metadata?.modelFailure)?.metadata?.modelFailure;
  assert.ok(diagnostic?.code && diagnostic.summary);
  const failureNotice = await waitFor(async () => (await pull(instance)).items
    .find(item => item.sessionId === sessionId && item.kind === 'failed'), 'original failure notification', 25_000);
  assert.equal(failureNotice.turn, 3);
  assert.equal(failureNotice.body, diagnostic.summary);
  assert.equal(failedRequests, 1);
  const settled = (await pull(instance)).items.filter(item => item.sessionId === sessionId);
  assert.deepEqual(settled.map(record => [record.turn, record.kind]), [[1, 'completed'], [3, 'failed']]);
  result.checks.failed = {
    providerHttpStatus: 401, canonicalStatus: failed.status, nativeCode: diagnostic.code,
    notificationKind: failureNotice.kind, matchedNativeSummary: true, turn: 3,
  };
  result.checks.cancelled.noCompletionOrFailureNotification = true;
  console.log('Notify preserved the original failure summary and did not announce the cancelled turn.');

  const pluginState = JSON.parse(await readFile(join(state, 'plugins/state.json')));
  const lock = await readFile(join(pluginState.loading.configuration.preparation.projectDirectory, 'package-lock.json'));
  result.dependencyLockSha256 = sha256(lock);
  await writeFile(join(output, 'package-lock.json'), lock);
} catch (error) {
  failure = error;
  result.failure = error.message;
} finally {
  page?.close();
  if (app) await app.stop();
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await writeFile(join(output, 'result.json'), JSON.stringify(result, null, 2) + '\n');
}
if (failure) throw failure;
