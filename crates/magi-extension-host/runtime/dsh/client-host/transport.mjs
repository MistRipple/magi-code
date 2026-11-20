const CHANNEL = 'magi-plugin-v1';
const VIRTUAL_ORIGIN = 'http://dsh.local';

function bytesToBase64(bytes) {
  let s = '';
  for (let i = 0; i < bytes.byteLength; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}

function base64ToBytes(encoded) {
  const raw = atob(encoded);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

export function createHostBridge({ onInit, onContext, onClose }) {
  let instanceId = null;
  const pending = new Map();
  const sockets = new Map();

  const post = payload => {
    if (!instanceId || typeof window === 'undefined' || !window.parent || window.parent === window) return;
    window.parent.postMessage({ channel: CHANNEL, instanceId, ...payload }, '*');
  };

  function createWebSocket(rawUrl) {
    const parsed = new URL(String(rawUrl), `${VIRTUAL_ORIGIN}/`);
    const socketId = crypto.randomUUID();
    const listeners = { open: new Set(), message: new Set(), error: new Set(), close: new Set() };
    let readyState = 0; // CONNECTING
    const dispatch = (type, event) => {
      const set = listeners[type];
      if (!set) return;
      for (const entry of [...set]) {
        if (entry.once) set.delete(entry);
        try { entry.listener(event); } catch (err) { console.error('[dsh-client-host] ws listener failed:', err); }
      }
    };
    const socket = {
      get readyState() { return readyState; },
      send(data) {
        if (readyState !== 1) return;
        post({ type: 'ws.send', socketId, data: String(data) });
      },
      close(code, reason) {
        if (readyState >= 2) return;
        readyState = 2; // CLOSING
        post({
          type: 'ws.close',
          socketId,
          ...(typeof code === 'number' ? { code } : {}),
          ...(typeof reason === 'string' ? { reason } : {}),
        });
      },
      addEventListener(type, listener, options) {
        if (!listeners[type] || typeof listener !== 'function') return;
        listeners[type].add({ listener, once: Boolean(options?.once) });
      },
      removeEventListener(type, listener) {
        const set = listeners[type];
        if (!set) return;
        for (const entry of set) {
          if (entry.listener === listener) { set.delete(entry); break; }
        }
      },
      _onOpen() {
        if (readyState !== 0) return;
        readyState = 1;
        dispatch('open', {});
      },
      _onMessage(data) {
        if (readyState !== 1) return;
        dispatch('message', { data });
      },
      _onError() {
        dispatch('error', {});
      },
      _onClose(code, reason) {
        if (readyState === 3) return;
        readyState = 3;
        sockets.delete(socketId);
        dispatch('close', { code, reason });
      },
    };
    sockets.set(socketId, socket);
    if (!instanceId) {
      queueMicrotask(() => {
        socket._onError();
        socket._onClose(1006, 'bridge not initialized');
      });
    } else {
      post({ type: 'ws.open', socketId, path: parsed.pathname });
    }
    return socket;
  }

  function request(payload, signal) {
    if (!instanceId) return Promise.reject(new Error('Plugin host bridge is not initialized'));
    if (signal?.aborted) return Promise.reject(signal.reason ?? new DOMException('Aborted', 'AbortError'));
    const requestId = crypto.randomUUID();
    return new Promise((resolve, reject) => {
      const onAbort = () => {
        pending.delete(requestId);
        post({ type: 'cancel', requestId });
        reject(signal.reason ?? new DOMException('Aborted', 'AbortError'));
      };
      if (signal) signal.addEventListener('abort', onAbort, { once: true });
      pending.set(requestId, {
        resolve: v => { if (signal) signal.removeEventListener('abort', onAbort); resolve(v); },
        reject: e => { if (signal) signal.removeEventListener('abort', onAbort); reject(e); },
      });
      post({ type: 'request', requestId, ...payload });
    });
  }

  function handleWindowMessage(event) {
    if (typeof window !== 'undefined' && event.source !== window.parent) return;
    const data = event.data;
    if (!data || data.channel !== CHANNEL || typeof data.instanceId !== 'string') return;
    if (data.type === 'init') {
      instanceId = data.instanceId;
      onInit?.(data);
      return;
    }
    if (data.instanceId !== instanceId) return;
    if (data.type === 'context') {
      onContext?.(data);
      return;
    }
    if (data.type === 'prepareClose') {
      Promise.resolve(onClose?.()).finally(() => post({ type: 'closeReady' }));
      return;
    }
    if (data.type === 'ws.open' && typeof data.socketId === 'string') {
      sockets.get(data.socketId)?._onOpen();
      return;
    }
    if (data.type === 'ws.message' && typeof data.socketId === 'string') {
      sockets.get(data.socketId)?._onMessage(data.data);
      return;
    }
    if (data.type === 'ws.error' && typeof data.socketId === 'string') {
      sockets.get(data.socketId)?._onError();
      return;
    }
    if (data.type === 'ws.close' && typeof data.socketId === 'string') {
      sockets.get(data.socketId)?._onClose(data.code, data.reason);
      return;
    }
    if (data.type === 'response' && typeof data.requestId === 'string') {
      const waiter = pending.get(data.requestId);
      if (!waiter) return;
      pending.delete(data.requestId);
      if (data.ok) waiter.resolve(data.result);
      else waiter.reject(new Error(typeof data.error === 'string' ? data.error : 'Host request failed'));
    }
  }

  async function routedFetch(input, init = {}) {
    const rawUrl = input instanceof Request ? input.url : String(input);
    const parsed = new URL(rawUrl, `${VIRTUAL_ORIGIN}/`);
    if (parsed.origin !== VIRTUAL_ORIGIN && parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      throw new TypeError(`Unsupported URL scheme: ${parsed.protocol}`);
    }
    if (
      parsed.origin !== VIRTUAL_ORIGIN &&
      (typeof location === 'undefined' || parsed.origin !== location.origin || location.origin === 'null')
    ) {
      throw new TypeError(`Sandboxed plugin page cannot fetch external origin directly: ${parsed.origin}`);
    }
    const method = (init.method ?? (input instanceof Request ? input.method : 'GET')).toUpperCase();
    const headersObj = {};
    new Headers(init.headers ?? (input instanceof Request ? input.headers : undefined)).forEach((v, k) => {
      headersObj[k] = v;
    });

    let body;
    let bodyBase64;
    const rawBody =
      init.body !== undefined
        ? init.body
        : input instanceof Request && method !== 'GET' && method !== 'HEAD'
          ? await input.arrayBuffer()
          : undefined;
    if (typeof rawBody === 'string') {
      if ((headersObj['content-type'] ?? '').toLowerCase().includes('application/json')) {
        try { body = JSON.parse(rawBody); } catch { bodyBase64 = bytesToBase64(new TextEncoder().encode(rawBody)); }
      } else {
        bodyBase64 = bytesToBase64(new TextEncoder().encode(rawBody));
      }
    } else if (rawBody instanceof ArrayBuffer) {
      bodyBase64 = bytesToBase64(new Uint8Array(rawBody));
    } else if (ArrayBuffer.isView(rawBody)) {
      bodyBase64 = bytesToBase64(new Uint8Array(rawBody.buffer, rawBody.byteOffset, rawBody.byteLength));
    } else if (rawBody instanceof Blob) {
      bodyBase64 = bytesToBase64(new Uint8Array(await rawBody.arrayBuffer()));
    } else if (rawBody instanceof URLSearchParams) {
      if (!headersObj['content-type']) headersObj['content-type'] = 'application/x-www-form-urlencoded;charset=UTF-8';
      bodyBase64 = bytesToBase64(new TextEncoder().encode(rawBody.toString()));
    }

    const query = parsed.search.startsWith('?') ? parsed.search.slice(1) : '';
    const documentUrl = new URL(location.href);
    const documentPrefix = documentUrl.pathname + '/';
    const documentDirectory = new URL('.', documentUrl).pathname;
    // Both route URLs rooted at the transport origin and URLs resolved using
    // the plugin document's own directory address the same private web app.
    let path = parsed.pathname;
    if (parsed.origin === documentUrl.origin) {
      const documentBase = [documentPrefix, documentDirectory].find(prefix => path.startsWith(prefix));
      if (documentBase) path = '/' + path.slice(documentBase.length);
    }
    const routed = await request({
      operation: 'route.call',
      method,
      path,
      ...(query ? { query } : {}),
      ...(Object.keys(headersObj).length ? { headers: headersObj } : {}),
      ...(body !== undefined ? { body } : {}),
      ...(bodyBase64 !== undefined ? { bodyBase64 } : {}),
    }, init.signal);

    const status = typeof routed?.status === 'number' ? routed.status : 200;
    const responseHeaders = new Headers(routed?.headers ?? {});
    let responseBytes = null;
    if (typeof routed?.bodyBase64 === 'string') {
      responseBytes = base64ToBytes(routed.bodyBase64);
    } else if (routed?.body !== undefined && status !== 204 && status !== 304) {
      if (!responseHeaders.has('content-type')) responseHeaders.set('content-type', 'application/json; charset=utf-8');
      responseBytes = new TextEncoder().encode(JSON.stringify(routed.body));
    }
    return new Response(status === 204 || status === 304 ? null : responseBytes, {
      status,
      headers: responseHeaders,
    });
  }

  const localMap = new Map();
  const sessionMap = new Map();

  const storageWrites = new Set();
  function writeStorage(area, mutation) {
    if (!instanceId) return;
    const write = request({ operation: 'view.storage.write', area, mutation }).catch(() => {});
    storageWrites.add(write);
    void write.then(() => storageWrites.delete(write));
  }

  function createVirtualStorage(area, map) {
    const storageApi = {
      get length() {
        return map.size;
      },
      key(index) {
        const keys = [...map.keys()];
        return Number.isInteger(index) && index >= 0 && index < keys.length ? keys[index] : null;
      },
      getItem(key) {
        const k = String(key);
        return map.has(k) ? map.get(k) : null;
      },
      setItem(key, value) {
        const k = String(key);
        const v = String(value);
        map.set(k, v);
        writeStorage(area, { kind: 'set', key: k, value: v });
      },
      removeItem(key) {
        const k = String(key);
        map.delete(k);
        writeStorage(area, { kind: 'remove', key: k });
      },
      clear() {
        map.clear();
        writeStorage(area, { kind: 'clear' });
      },
    };
    return new Proxy(storageApi, {
      get(target, prop, receiver) {
        if (prop in target || typeof prop === 'symbol') return Reflect.get(target, prop, receiver);
        return map.has(prop) ? map.get(prop) : undefined;
      },
      set(target, prop, value) {
        if (prop in target || typeof prop === 'symbol') return false;
        target.setItem(prop, value);
        return true;
      },
      deleteProperty(target, prop) {
        if (typeof prop === 'symbol' || prop in target) return false;
        target.removeItem(prop);
        return true;
      },
      has(target, prop) {
        return prop in target || (typeof prop === 'string' && map.has(prop));
      },
      ownKeys() {
        return [...map.keys()];
      },
      getOwnPropertyDescriptor(_target, prop) {
        if (typeof prop === 'string' && map.has(prop)) {
          return { value: map.get(prop), writable: true, enumerable: true, configurable: true };
        }
        return undefined;
      },
    });
  }

  const virtualLocalStorage = createVirtualStorage('local', localMap);
  const virtualSessionStorage = createVirtualStorage('session', sessionMap);

  async function hydrateStorage() {
    try {
      const snap = await request({ operation: 'view.storage.read' });
      if (snap?.local && typeof snap.local === 'object') {
        localMap.clear();
        for (const [k, v] of Object.entries(snap.local)) {
          if (typeof v === 'string') localMap.set(k, v);
        }
      }
      if (snap?.session && typeof snap.session === 'object') {
        sessionMap.clear();
        for (const [k, v] of Object.entries(snap.session)) {
          if (typeof v === 'string') sessionMap.set(k, v);
        }
      }
    } catch {
      // Keep empty in-memory maps if host storage read fails.
    }
  }

  function installBrowserHooks() {
    if (typeof window === 'undefined') return;
    try {
      Object.defineProperty(window, 'localStorage', {
        configurable: true,
        enumerable: true,
        get: () => virtualLocalStorage,
      });
    } catch {}
    try {
      Object.defineProperty(window, 'sessionStorage', {
        configurable: true,
        enumerable: true,
        get: () => virtualSessionStorage,
      });
    } catch {}
    try {
      const clip = {
        writeText: text => request({ operation: 'clipboard.write', text: String(text ?? '') }),
      };
      Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        get: () => clip,
      });
    } catch {}
    window.addEventListener('message', handleWindowMessage);
    window.fetch = routedFetch;
    window.open = url => {
      if (typeof url === 'string' && url) void request({ operation: 'host.openExternal', url }).catch(() => {});
      return null;
    };
    document.addEventListener('click', ev => {
      if (ev.defaultPrevented) return;
      const anchor = ev.target instanceof Element ? ev.target.closest('a[href]') : null;
      const href = anchor?.getAttribute('href') ?? '';
      if (/^(?:https?:|mailto:)/i.test(href)) {
        ev.preventDefault();
        void request({ operation: 'host.openExternal', url: href }).catch(() => {});
      }
    });
    globalThis.__DSH_TRANSPORT__ = {
      ownsHost: true,
      streamBaseUrl: `${VIRTUAL_ORIGIN}/`,
      fetch: routedFetch,
      createWebSocket,
    };
  }

  let slotReportChain = Promise.resolve();
  const bridge = {
    get isInitialized() { return instanceId !== null; },
    installBrowserHooks,
    hydrateStorage,
    flushStorage: () => Promise.all([...storageWrites]),
    request,
    routedFetch,
    reportHeight: height => request({ operation: 'view.resize', height }).catch(() => {}),
    reportSlots: summary => {
      slotReportChain = slotReportChain
        .then(() => request({ operation: 'slots.report', ...summary }))
        .catch(() => {});
      return slotReportChain;
    },
    closeSettings: () => request({ operation: 'settings.close' }).catch(() => {}),
    installPlugin: spec => request({ operation: 'plugins.install', spec }),
    updatePlugin: (packageName, version) => request({ operation: 'plugins.update', packageName, version }),
    openPackage: packageName => request({ operation: 'plugins.openPackage', packageName }).catch(() => {}),
  };

  return bridge;
}
