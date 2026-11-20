import { timingSafeEqual } from 'node:crypto';
import { Service } from '@deepseek-ai/cordis';
import { WebServer } from '@deepseek-ai/dsh-host-webserver';

const proofHeader = 'x-magi-route-proof';
const instanceHeader = 'x-magi-route-instance';
const generationHeader = 'x-magi-route-generation';
const epochHeader = 'x-magi-route-epoch';
const sessionHeader = 'x-magi-route-session';
const transportHeaders = new Set([proofHeader, instanceHeader, generationHeader, epochHeader, sessionHeader]);

function sameProof(actual, expected) {
  if (typeof actual !== 'string' || !/^[a-f0-9]{64}$/.test(actual)) return false;
  return timingSafeEqual(Buffer.from(actual, 'hex'), expected);
}

function validPath(path) {
  if (typeof path !== 'string' || !path.startsWith('/') || path.includes('\\')) return false;
  const parsed = new URL(path, 'http://plugin.invalid');
  return parsed.origin === 'http://plugin.invalid' && parsed.pathname === path && !parsed.search && !parsed.hash
    && (path === '/' || !path.endsWith('/'));
}

function deny(response, status) {
  response.writeHead(status);
  response.end();
}

function removeTransportHeaders(request) {
  for (const name of transportHeaders) {
    delete request.headers[name];
    delete request.headersDistinct[name];
  }
  for (let i = request.rawHeaders.length - 2; i >= 0; i -= 2) {
    if (transportHeaders.has(request.rawHeaders[i].toLowerCase())) request.rawHeaders.splice(i, 2);
  }
}

/** Keep resource backpressure and cancel the reader when its owner is revoked. */
function bindResourceBody(response, signal) {
  if (!response.body) return response;
  const reader = response.body.getReader();
  let cancel;
  const detach = () => signal.removeEventListener('abort', cancel);
  return new Response(new ReadableStream({
    start(controller) {
      cancel = () => {
        detach();
        controller.error(signal.reason);
        void reader.cancel(signal.reason).catch(() => {});
      };
      signal.addEventListener('abort', cancel, { once: true });
      if (signal.aborted) cancel();
    },
    async pull(controller) {
      try {
        const chunk = await reader.read();
        if (signal.aborted) return;
        if (chunk.done) { detach(); controller.close(); }
        else controller.enqueue(chunk.value);
      } catch (error) {
        detach();
        if (!signal.aborted) controller.error(error);
      }
    },
    cancel(reason) { detach(); return reader.cancel(reason); },
  }), { status: response.status, statusText: response.statusText, headers: response.headers });
}

async function runResource(resource, request, peer, signal) {
  const pending = Promise.resolve().then(() => {
    signal.throwIfAborted();
    return resource(request, peer);
  });
  const stopped = Promise.withResolvers();
  const abort = () => stopped.reject(signal.reason);
  signal.addEventListener('abort', abort, { once: true });
  if (signal.aborted) abort();
  try {
    const response = await Promise.race([pending, stopped.promise]);
    signal.throwIfAborted();
    return bindResourceBody(response, signal);
  } catch (error) {
    if (!signal.aborted) throw error;
    // A late response belongs to the revoked call and cannot start a stream.
    void pending.then(response => response.body?.cancel(signal.reason)).catch(() => {});
    return new Response(null, { status: 409 });
  } finally {
    signal.removeEventListener('abort', abort);
  }
}

/**
 * Private HTTP transport for the daemon gateway. The locked DSH WebServer
 * owns matching, HTTP request/response objects and stream transport. Only a
 * request admitted for the active registration can reach plugin code.
 */
export async function createWebRouteCarrier(lifecycle, proof, sessionReader) {
  if (typeof proof !== 'string' || !/^[a-f0-9]{64}$/.test(proof)) throw new Error('Invalid private HTTP transport proof');
  const expected = Buffer.from(proof, 'hex');
  const context = lifecycle.context.isolate('webServer');
  let server;
  class PrivateWebServer extends WebServer {
    constructor(context, config) {
      super(context, config);
      server = this;
    }
  }
  const fiber = context.plugin(PrivateWebServer, { host: '127.0.0.1', port: 0 });
  try { await fiber.await(); }
  catch (error) { await fiber.dispose(); throw error; }
  const localReads = new WeakMap();

  function runSession(header, signal, handler) {
    signal.throwIfAborted();
    if (!header) return handler();
    if (!sessionReader || typeof header !== 'string') throw new Error('Invalid route session proof');
    return sessionReader.run(JSON.parse(Buffer.from(header, 'base64url').toString('utf8')), signal, handler);
  }

  function admit(scope, read, revoked) {
    if (!sameProof(read(proofHeader), expected)) return { status: 403 };
    if (read(instanceHeader) !== Buffer.from(scope.identity.instanceId).toString('base64url')) return { status: 404 };
    if (read(generationHeader) !== String(scope.identity.activationGeneration)) return { status: 409 };
    try {
      revoked.throwIfAborted();
      const lease = scope.acquire();
      if (read(epochHeader) !== String(lease.epoch)) return { status: 409 };
      return { signal: AbortSignal.any([lease.signal, revoked]) };
    } catch { return { status: 409 }; }
  }

  function decodeInstanceHeader(raw) {
    if (typeof raw !== 'string' || !raw) return undefined;
    if (raw === '$host') return '$host';
    try {
      const decoded = Buffer.from(raw, 'base64url').toString('utf8');
      if (!decoded || Buffer.from(decoded, 'utf8').toString('base64url') !== raw) return undefined;
      return decoded;
    } catch {
      return undefined;
    }
  }

  function admitHost(read, revoked) {
    if (!sameProof(read(proofHeader), expected)) return { status: 403 };
    const instanceId = decodeInstanceHeader(read(instanceHeader));
    if (!instanceId) return { status: 404 };
    const generation = Number(read(generationHeader));
    const epoch = Number(read(epochHeader));
    if (!Number.isSafeInteger(generation) || generation <= 0 || !Number.isSafeInteger(epoch) || epoch <= 0) {
      return { status: 409 };
    }
    try {
      revoked.throwIfAborted();
      const lease = lifecycle.acquireAnyInstance(instanceId, generation, epoch);
      return { signal: AbortSignal.any([lease.signal, revoked]) };
    } catch {
      return { status: 409 };
    }
  }

  function ownHost(context, name, register) {
    const revoke = new AbortController();
    return context.effect(() => {
      const unregister = register(revoke.signal);
      return () => {
        revoke.abort(new Error('Host route registration ended'));
        unregister();
      };
    }, name);
  }

  function wrapHostResponder(revoked, { handler, resource }) {
    if (handler) return { async handler(request, response) {
      const admission = admitHost(name => request.headers[name], revoked);
      if (!admission.signal) { deny(response, admission.status); return; }
      removeTransportHeaders(request);
      const abort = () => response.destroy();
      admission.signal.addEventListener('abort', abort, { once: true });
      response.once('close', () => admission.signal.removeEventListener('abort', abort));
      await handler(request, response);
    } };
    return { async resource(request, peer) {
      const admission = admitHost(name => request.headers.get(name), revoked);
      if (!admission.signal) return new Response(null, { status: admission.status });
      const signal = AbortSignal.any([request.signal, admission.signal]);
      const headers = new Headers(request.headers);
      for (const name of transportHeaders) headers.delete(name);
      return runResource(resource, new Request(request, { headers, signal }), peer, signal);
    } };
  }

  function own(context, name, register) {
    const scope = lifecycle.contributionScope(context);
    const revoke = new AbortController();
    return context.effect(() => {
      const unregister = register(scope, revoke.signal);
      return () => {
        revoke.abort(new Error('Plugin route registration ended'));
        unregister();
      };
    }, name);
  }

  function wrapResponder(scope, revoked, { handler, resource }) {
    if (handler) return { async handler(request, response) {
      const admission = admit(scope, name => request.headers[name], revoked);
      if (!admission.signal) { deny(response, admission.status); return; }
      const session = request.headers[sessionHeader];
      removeTransportHeaders(request);
      const lifetime = new AbortController();
      const abort = () => response.destroy();
      admission.signal.addEventListener('abort', abort, { once: true });
      response.once('close', () => {
        admission.signal.removeEventListener('abort', abort);
        lifetime.abort(new Error('Plugin route has closed'));
      });
      await runSession(session, AbortSignal.any([admission.signal, lifetime.signal]), () => handler(request, response));
    } };
    return { async resource(request, peer) {
      const local = localReads.get(request);
      let signal;
      if (local) {
        if (local.identity.instanceId !== scope.identity.instanceId
          || local.identity.activationGeneration !== scope.identity.activationGeneration) {
          return new Response(null, { status: 404 });
        }
        try { signal = AbortSignal.any([local.signal, scope.acquire().signal, revoked]); }
        catch { return new Response(null, { status: 409 }); }
      } else {
        const admission = admit(scope, name => request.headers.get(name), revoked);
        if (!admission.signal) return new Response(null, { status: admission.status });
        signal = admission.signal;
      }
      signal = AbortSignal.any([request.signal, signal]);
      const headers = new Headers(request.headers);
      const session = local ? null : headers.get(sessionHeader);
      for (const name of transportHeaders) headers.delete(name);
      return runSession(session, signal, () => runResource(resource, new Request(request, { headers, signal }), peer, signal));
    } };
  }

  function checkResponder({ handler, resource }) {
    if (typeof handler === 'function' ? resource !== undefined : handler !== undefined || typeof resource !== 'function') {
      throw new Error('A plugin route requires exactly one handler or resource');
    }
  }

  return Object.freeze({
    origin: server.getOrigin(),
    port: server.port,
    register(context, route) {
      const { path, kind } = route;
      if (!validPath(path) || !['exact', 'prefix'].includes(kind)) throw new Error('Invalid plugin HTTP route');
      checkResponder(route);
      const caller = lifecycle.context.reflect.trace(context);
      if (!lifecycle.hasActivation(caller)) {
        return ownHost(caller, `webServer:host:${kind}:${path}`, revoked =>
          server.register({ kind, path, ...wrapHostResponder(revoked, route) }));
      }
      return own(context, `webServer:${kind}:${path}`, (scope, revoked) =>
        server.register({ kind, path, ...wrapResponder(scope, revoked, route) }));
    },
    registerFallback(context, fallback) {
      const route = typeof fallback === 'function' ? { handler: fallback } : fallback;
      checkResponder(route);
      return own(context, 'webServer:fallback', (scope, revoked) => {
        const wrapped = wrapResponder(scope, revoked, route);
        return server.registerFallback(wrapped.handler ?? wrapped);
      });
    },
    registerUpgrade(context, { path, handler }) {
      if (!validPath(path) || typeof handler !== 'function') throw new Error('Invalid plugin HTTP upgrade route');
      const caller = lifecycle.context.reflect.trace(context);
      if (!lifecycle.hasActivation(caller)) {
        return ownHost(caller, `webServer:host:upgrade:${path}`, revoked => server.registerUpgrade({
          path,
          async handler(request, socket, head) {
            const admission = admitHost(name => request.headers[name], revoked);
            if (!admission.signal) { socket.destroy(); return; }
            removeTransportHeaders(request);
            const abort = () => socket.destroy();
            admission.signal.addEventListener('abort', abort, { once: true });
            socket.once('close', () => admission.signal.removeEventListener('abort', abort));
            await handler(request, socket, head);
          },
        }));
      }
      return own(context, `webServer:upgrade:${path}`, (scope, revoked) => server.registerUpgrade({
        path,
        async handler(request, socket, head) {
          const admission = admit(scope, name => request.headers[name], revoked);
          if (!admission.signal) { socket.destroy(); return; }
          removeTransportHeaders(request);
          const abort = () => socket.destroy();
          admission.signal.addEventListener('abort', abort, { once: true });
          socket.once('close', () => admission.signal.removeEventListener('abort', abort));
          await handler(request, socket, head);
        },
      }));
    },
    readResource(context, request, peer) {
      const scope = lifecycle.contributionScope(context);
      const { signal } = scope.acquire();
      // Only the Request issued by this admitted caller carries local access.
      const read = new Request(request);
      localReads.set(read, { identity: scope.identity, signal });
      return server.readResource(read, peer);
    },
    tapIndex(context, transform) {
      if (typeof transform !== 'function') throw new Error('Invalid plugin index transform');
      return own(context, 'webServer:tapIndex', (scope, revoked) => server.tapIndex(html => {
        if (revoked.aborted) return html;
        try { scope.acquire(); } catch { return html; }
        return transform(html);
      }));
    },
    applyIndexTaps: html => server.applyIndexTaps(html),
    collectIndexInjections: peer => server.collectIndexInjections(peer),
    renderIndex: (html, peer) => server.renderIndex(html, peer),
    close: () => fiber.dispose(),
  });
}

/** Publish only when the daemon owns the public gateway for this process. */
export async function openWebRoutes(lifecycle, proof, publicOrigin, sessionReader) {
  const origin = new URL(publicOrigin);
  if (!['http:', 'https:'].includes(origin.protocol) || origin.origin !== publicOrigin) {
    throw new Error('Plugin gateway requires an HTTP origin');
  }
  const carrier = await createWebRouteCarrier(lifecycle, proof, sessionReader);
  class PluginWebServer extends Service {
    constructor(context) { super(context, 'webServer'); }
    get port() { return Number(origin.port || (origin.protocol === 'https:' ? 443 : 80)); }
    get protocol() { return origin.protocol; }
    get host() { return origin.hostname; }
    getOrigin() { return origin.origin; }
    register(route) { return carrier.register(this.ctx, route); }
    registerFallback(route) { return carrier.registerFallback(this.ctx, route); }
    registerUpgrade(route) { return carrier.registerUpgrade(this.ctx, route); }
    readResource(request, peer) { return carrier.readResource(this.ctx, request, peer); }
    tapIndex(transform) { return carrier.tapIndex(this.ctx, transform); }
    applyIndexTaps(html) { return carrier.applyIndexTaps(html); }
    collectIndexInjections(peer) { return carrier.collectIndexInjections(peer); }
    renderIndex(html, peer) { return carrier.renderIndex(html, peer); }
  }
  const service = lifecycle.context.plugin(PluginWebServer);
  try { await service.await(); }
  catch (error) { await service.dispose(); await carrier.close(); throw error; }
  return Object.freeze({ port: carrier.port, async close() {
    try { await service.dispose(); } finally { await carrier.close(); }
  } });
}
