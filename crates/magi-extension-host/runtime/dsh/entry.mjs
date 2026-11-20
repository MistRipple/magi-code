// Internal shared-host control endpoint. Plugin packages only see the Cordis contract.
import net from 'node:net';
import { createHash } from 'node:crypto';
import { isAbsolute } from 'node:path';
import { pathToFileURL } from 'node:url';
import { PluginLifecycle } from './lifecycle.mjs';
import { inspectOriginalPackage } from './packages.mjs';
import { LoadingSourcesRequired, parseConfigurationSource, prepareConfiguration } from './configuration.mjs';
import { CommittedTree } from './committed-tree.mjs';
import { prepareConfigurationEdit } from './configuration-edit.mjs';
import { openPluginManager, prepareBundleMutation } from './plugin-manager.mjs';
import { openHostServices } from './services.mjs';
import { publishCommittedSessionEvent } from './session-observer.mjs';
import { createSessionReader } from './session-reader.mjs';
import { openWebRoutes } from './web-routes.mjs';

const endpoint = process.env.MAGI_EXT_ENDPOINT;
const token = process.env.MAGI_EXT_TOKEN;
delete process.env.MAGI_EXT_TOKEN;
if (!endpoint || !token) throw new Error('Missing shared-host endpoint or token');

const MAX_FRAME = 4 * 1024 * 1024;
const MAX_PENDING = 256;
let frameLimit = MAX_FRAME, pendingLimit = MAX_PENDING;
let buffer = Buffer.alloc(0), host, tree, closing = false, pending = 0;
let webRoutes, webOrigin, pluginManagerService, hostServices, hostServicesHome;
let nextRequest = 0;
const sessionReader = createSessionReader(requestOwner);
const requests = new Map();
const cancellableRequests = new Map();
const cancellableServices = {
  'host.tools.execute': 'executeTool',
  'host.tools.preExecute': 'preExecuteTool',
  'host.tools.postExecute': 'postExecuteTool',
  'host.systemPrompt.assemble': 'assembleSystemPrompt',
  'host.approval.ask': 'askApproval',
  'host.compaction.before': 'beforeCompaction',
  'host.session.projectMessages': 'projectMessages',
  'host.llm.extendRequest': 'extendLlmRequest',
  'host.llm.interceptStream': 'interceptLlmStream',
  'host.agentLoop.run': 'runAgentLoop',
  'host.agentLoop.resume': 'resumeAgentLoop',
  'host.web.search': 'webSearch',
  'host.web.fetch': 'webFetch',
  'host.commands.execute': 'executeCommand',
};
const socket = net.createConnection(endpoint);
const rpcError = (kind, message, detail = null) => ({ code: -32000, message, data: { kind, retryable: false, detail } });

function send(message) {
  const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...message }));
  if (body.length > frameLimit || socket.writableLength + body.length + 4 > frameLimit * 4) {
    throw new Error('Shared-host transport capacity exceeded');
  }
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length);
  socket.write(Buffer.concat([header, body]));
}

function requestOwner(method, params) {
  if (closing || socket.destroyed) return Promise.reject(new Error('Shared-host owner disconnected'));
  if (requests.size >= pendingLimit) return Promise.reject(new Error('Shared-host owner request limit exceeded'));
  const id = `h${++nextRequest}`;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      requests.delete(id);
      reject(new Error(`${method} timed out; the saved outcome is unconfirmed`));
    }, 120_000);
    requests.set(id, { resolve, reject, timer });
    try { send({ id, method, params }); }
    catch (error) { clearTimeout(timer); requests.delete(id); reject(error); }
  });
}

function rejectOwnerRequests(error) {
  for (const request of requests.values()) { clearTimeout(request.timer); request.reject(error); }
  requests.clear();
}

function terminate(error) {
  if (closing) return;
  closing = true;
  rejectOwnerRequests(error);
  process.stderr.write(`magi-plugin-host: ${error.message}\n`);
  socket.destroy();
  // The owner has disconnected. Draining must not leave an orphaned process.
  const deadline = setTimeout(() => process.exit(2), 5000);
  closeRuntime().catch(failure => process.stderr.write(`${failure.message}\n`)).finally(() => {
    clearTimeout(deadline);
    process.exit(2);
  });
}

async function closeRuntime() {
  for (const controller of cancellableRequests.values()) controller.abort();
  try { await pluginManagerService?.then(service => service.close(), () => {}); } finally {
    try { await tree?.close(); } finally {
      try { await hostServices?.then(service => service.close(), () => {}); } finally {
        try { await host?.close(); } finally {
          await webRoutes?.then(routes => routes.close(), () => {});
        }
      }
    }
  }
}

async function requireServices() {
  if (!hostServices) throw new Error('Shared-host services are not open');
  return await hostServices;
}

async function dispatch(message) {
  if (message.jsonrpc !== '2.0') throw new Error('Invalid JSON-RPC envelope');
  if (!host) {
    if (message.id !== 'hello' || message.error || message.result?.protocol?.major !== 1
      || !message.result.features?.includes('runtime.shared')) throw new Error('Shared-host handshake rejected');
    const { limits, hostGeneration } = message.result;
    if (!Number.isSafeInteger(limits?.frameBytes) || limits.frameBytes < 1024 || limits.frameBytes > MAX_FRAME
      || !Number.isSafeInteger(limits.pending) || limits.pending < 1 || limits.pending > MAX_PENDING) {
      throw new Error('Invalid shared-host transport limits');
    }
    frameLimit = limits.frameBytes;
    pendingLimit = limits.pending;
    host = new PluginLifecycle({
      hostGeneration,
      onStatus: status => send({ method: 'plugin.status', params: status }),
      onDiagnostic: diagnostic => send({ method: 'host.diagnostic', params: diagnostic }),
    });
    tree = new CommittedTree({ lifecycle: host, hostGeneration, requestOwner, pendingLimit,
      onDiagnostic: diagnostic => send({ method: 'host.diagnostic', params: diagnostic }) });
    send({ method: 'host.ready', params: { hostGeneration } });
    return;
  }
  if (typeof message.id === 'string' && message.method === undefined) {
    if (Object.hasOwn(message, 'result') === Object.hasOwn(message, 'error')) throw new Error('Invalid owner response');
    const request = requests.get(message.id);
    if (!request) return; // A timed-out response cannot revive its caller.
    requests.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(Object.assign(new Error(message.error.message), { kind: message.error.data?.kind }));
    else request.resolve(message.result);
    return;
  }
  if (message.id === undefined && message.method === 'request.cancel') {
    cancellableRequests.get(message.params?.id)?.abort();
    return;
  }
  if (message.id === undefined && message.method === 'host.session.committed') {
    if (message.params?.hostGeneration !== host.generation || closing) return;
    try { publishCommittedSessionEvent(host.context, message.params); }
    catch (error) { host.context.logger.warn(error); }
    hostServices?.then(services => services.observeSessionCommitted(message.params)).catch(() => {});
    return;
  }
  if (message.id === undefined && message.method === 'host.session.changed') {
    if (message.params?.hostGeneration !== host.generation || closing) return;
    hostServices?.then(services => services.observeSessionChanged(message.params)).catch(() => {});
    return;
  }
  if (message.id === undefined && message.method === 'host.session.assistantStream') {
    if (message.params?.hostGeneration !== host.generation || closing) return;
    hostServices?.then(services => services.observeAssistantStream(message.params)).catch(() => {});
    return;
  }
  if (typeof message.id !== 'string' || !message.id || typeof message.method !== 'string') {
    throw new Error('Expected a shared-host control request');
  }
  if (pending >= pendingLimit) {
    send({ id: message.id, error: rpcError('Backpressure', 'Shared-host request limit exceeded') });
    return;
  }
  pending++;
  let controller;
  try {
    let result;
    const params = message.params ?? {};
    if (Object.hasOwn(cancellableServices, message.method)) {
      controller = new AbortController();
      cancellableRequests.set(message.id, controller);
      const services = await requireServices();
      controller.signal.throwIfAborted();
      result = await services[cancellableServices[message.method]](params, controller.signal);
      controller.signal.throwIfAborted();
    } else switch (message.method) {
      case 'host.ping': result = { rssBytes: process.memoryUsage.rss() }; break;
      case 'host.webRoutes.open': {
        const { publicOrigin } = params;
        if (typeof publicOrigin !== 'string' || !publicOrigin || (webRoutes && webOrigin !== publicOrigin)) {
          throw new Error('Plugin gateway origin is invalid or already bound');
        }
        if (!webRoutes) {
          webOrigin = publicOrigin;
          const proof = createHash('sha256').update('magi:private-web-routes:v1\0').update(token).digest('hex');
          webRoutes = openWebRoutes(host, proof, publicOrigin, sessionReader).catch(error => {
            webRoutes = undefined;
            webOrigin = undefined;
            throw error;
          });
        }
        result = { port: (await webRoutes).port };
        break;
      }
      case 'host.pluginManager.open': {
        if (!pluginManagerService) {
          pluginManagerService = openPluginManager({
            lifecycle: host,
            tree,
            hostGeneration: host.generation,
            requestOwner,
          }).catch(error => {
            pluginManagerService = undefined;
            throw error;
          });
        }
        await pluginManagerService;
        result = {};
        break;
      }
      case 'host.services.open': {
        const { dshHome, initialHostConfig } = params;
        if (typeof dshHome !== 'string' || !isAbsolute(dshHome) || (hostServices && hostServicesHome !== dshHome)) {
          throw new Error('Host services dshHome is invalid or already bound to another directory');
        }
        if (!hostServices) {
          hostServicesHome = dshHome;
          hostServices = openHostServices({
            lifecycle: host,
            tree,
            dshHome,
            initialHostConfig: initialHostConfig ?? {},
            requestOwner,
            sessionReader,
            notifyOwner: (method, payload) => send({
              method,
              params: { hostGeneration: host.generation, ...payload },
            }),
          }).catch(error => {
            hostServices = undefined;
            hostServicesHome = undefined;
            throw error;
          });
        }
        await hostServices;
        result = {};
        break;
      }
      case 'host.web.listProviders': result = (await requireServices()).listWebProviders(); break;
      case 'host.tools.filterVisible': result = await (await requireServices()).filterVisibleTools(params); break;
      case 'host.hooks.capabilities': result = (await requireServices()).hookCapabilities(); break;
      case 'host.agentLoop.cancel': result = await (await requireServices()).cancelAgentLoop(params); break;
      case 'host.skills.list': result = await (await requireServices()).listSkills(params); break;
      case 'host.skills.get': result = await (await requireServices()).getSkill(params); break;
      case 'host.llm.syncHostModels': result = await (await requireServices()).syncHostModels(params); break;
      case 'host.llm.refreshProviders': result = await (await requireServices()).refreshLlmProviders(); break;
      case 'host.settings.describe': result = (await requireServices()).describeSettings(); break;
      case 'host.settings.mutate': result = await (await requireServices()).mutateSettings(params); break;
      case 'host.settings.update': result = await (await requireServices()).updateSettings(params); break;
      case 'host.settings.replace': result = await (await requireServices()).replaceSettings(params); break;
      case 'package.inspect': result = await inspectOriginalPackage(params); break;
      case 'configuration.parse': result = parseConfigurationSource(params); break;
      case 'configuration.prepare': result = await prepareConfiguration(params); break;
      case 'configuration.edit.prepare': result = await prepareConfigurationEdit(params); break;
      case 'configuration.bundle.prepare': result = await prepareBundleMutation(params); break;
      case 'configuration.apply': result = await tree.apply(params); break;
      case 'plugin.activate':
        if (typeof params.entry !== 'string' || !isAbsolute(params.entry) || 'plugin' in params) throw new Error('Activation requires an absolute original package entry');
        result = await host.activate({ ...params, entry: pathToFileURL(params.entry).href });
        break;
      case 'plugin.deactivate': await host.deactivate(params); result = {}; break;
      case 'plugin.snapshot': result = host.snapshot(params); break;
      case 'host.shutdown':
        await closeRuntime();
        send({ id: message.id, result: {} });
        closing = true;
        rejectOwnerRequests(new Error('Shared plugin host shut down'));
        socket.end(() => process.exit(0));
        return;
      default:
        send({ id: message.id, error: rpcError('MethodNotFound', `Unknown shared-host method: ${message.method}`) });
        return;
    }
    send({ id: message.id, result });
  } catch (error) {
    if (host.requiresRestart || tree.requiresRestart) { terminate(error); return; }
    const detail = error instanceof LoadingSourcesRequired ? { requiredSources: error.sources } : null;
    if (!closing) send({ id: message.id, error: rpcError(controller?.signal.aborted ? 'Cancelled' : 'Conflict', error.message, detail) });
  } finally {
    if (controller) cancellableRequests.delete(message.id);
    pending--;
  }
}

socket.on('connect', () => send({ id: 'hello', method: 'host.hello', params: {
  token, protocol: { major: 1, minor: 0 }, features: ['runtime.shared'],
} }));
socket.on('data', data => {
  buffer = Buffer.concat([buffer, data]);
  try {
    while (!closing && buffer.length >= 4) {
      const length = buffer.readUInt32BE(0);
      if (!length || length > frameLimit) throw new Error('Invalid shared-host frame length');
      if (buffer.length < length + 4) break;
      const message = JSON.parse(buffer.subarray(4, length + 4).toString('utf8'));
      buffer = buffer.subarray(length + 4);
      void dispatch(message).catch(terminate);
    }
  } catch (error) { terminate(error); }
});
socket.on('error', terminate);
socket.on('close', () => terminate(new Error('Shared-host owner disconnected')));
process.on('uncaughtException', terminate);
process.on('unhandledRejection', error => terminate(error instanceof Error ? error : new Error(String(error))));
