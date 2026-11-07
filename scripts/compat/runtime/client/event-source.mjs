// EventSource 外观仅承载当前插件路由，不打开额外端口；帧仍由上游页面解释。
const STREAM_ROUTE = '/__magi_compat/streams';

export function createEventSource(request, baseUri, { retryMs = 3000, maxEventChars = 2 * 1024 * 1024 } = {}) {
  const live = new Set();
  const route = async (body, signal) => {
    const result = await request({ operation: 'route.call', method: 'POST', path: STREAM_ROUTE, body }, signal);
    if (result.status !== 200) throw new Error(result.body?.error ?? `事件连接失败：${result.status}`);
    return result.body;
  };
  class BridgeEventSource extends EventTarget {
    static CONNECTING = 0; static OPEN = 1; static CLOSED = 2;
    CONNECTING = 0; OPEN = 1; CLOSED = 2;
    readyState = 0;
    withCredentials = false;
    onopen = null; onmessage = null; onerror = null;
    #controller = new AbortController();
    #id; #lastId = ''; #retry = retryMs;
    constructor(raw, options = {}) {
      super();
      const url = new URL(String(raw), baseUri);
      if (url.origin !== new URL(baseUri).origin) throw new TypeError('事件连接只允许当前插件路由');
      if (options.withCredentials) throw new TypeError('插件事件连接不能携带宿主凭据');
      this.url = url.href;
      this.path = url.pathname; this.query = url.search.slice(1);
      live.add(this);
      void this.#run();
    }
    #emit(event) {
      this.dispatchEvent(event);
      const listener = this[`on${event.type}`];
      if (typeof listener === 'function') listener.call(this, event);
    }
    async #run() {
      const signal = this.#controller.signal;
      while (!signal.aborted) {
        let buffer = '', data = [], type = '', after = 0, pendingCR = false, first = true, eventChars = 0;
        const line = (value) => {
          if (!value) {
            if (data.length) this.#emit(new MessageEvent(type || 'message', { data: data.join('\n'), lastEventId: this.#lastId, origin: new URL(this.url).origin }));
            data = []; type = ''; eventChars = 0;
            return;
          }
          if (value.startsWith(':')) return;
          const colon = value.indexOf(':');
          const name = colon < 0 ? value : value.slice(0, colon);
          let text = colon < 0 ? '' : value.slice(colon + 1);
          if (text.startsWith(' ')) text = text.slice(1);
          if (name === 'data') { eventChars += text.length; if (eventChars > maxEventChars) throw new Error('插件事件超过大小上限'); data.push(text); }
          if (name === 'event') type = text;
          if (name === 'id' && !text.includes('\0')) this.#lastId = text;
          if (name === 'retry' && /^\d+$/.test(text)) this.#retry = Math.min(60_000, Math.max(1000, Number(text)));
        };
        const chunk = (text) => {
          for (const c of text) {
            if (first) { first = false; if (c === '\uFEFF') continue; }
            if (pendingCR) { pendingCR = false; if (c === '\n') continue; }
            if (c === '\r' || c === '\n') { line(buffer); buffer = ''; pendingCR = c === '\r'; }
            else { buffer += c; if (buffer.length + eventChars > maxEventChars) throw new Error('插件事件超过大小上限'); }
            if (signal.aborted) return;
          }
        };
        try {
          const opened = await route({ action: 'open', request: { method: 'GET', path: this.path, query: this.query,
            ...(this.#lastId ? { headers: { 'last-event-id': this.#lastId } } : {}) } }, signal);
          if (opened.status === 204) { this.close(); return; }
          this.#id = opened.id;
          if (signal.aborted) break;
          this.readyState = this.OPEN;
          this.#emit(new Event('open'));
          while (!signal.aborted) {
            const result = await route({ action: 'read', id: opened.id, after }, signal);
            if (signal.aborted) break;
            for (const entry of result.chunks) {
              if (entry.seq <= after) continue;
              if (entry.seq !== after + 1) throw new Error('插件事件传输序号不连续');
              chunk(entry.text); after = entry.seq;
            }
            if (result.error) throw new Error(result.error);
            if (result.ended) { this.readyState = this.CONNECTING; this.#emit(new Event('error')); break; }
          }
        } catch (error) {
          if (!signal.aborted) { this.readyState = this.CONNECTING; this.#emit(new Event('error')); }
        } finally {
          this.#release();
        }
        if (!signal.aborted) {
          this.readyState = this.CONNECTING;
          await new Promise((resolve) => {
            const done = () => { clearTimeout(timer); signal.removeEventListener('abort', done); resolve(); };
            const timer = setTimeout(done, this.#retry);
            signal.addEventListener('abort', done, { once: true });
          });
        }
      }
    }
    #release() {
      const id = this.#id; this.#id = undefined;
      if (id) void route({ action: 'close', id }).catch(() => {});
    }
    close() {
      if (this.readyState === this.CLOSED) return;
      this.readyState = this.CLOSED;
      this.#controller.abort();
      this.#release();
      live.delete(this);
    }
  }
  return { EventSource: BridgeEventSource, dispose() { for (const connection of live) connection.close(); } };
}
