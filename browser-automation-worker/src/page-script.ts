export const MAGI_AUTOMATION_WORLD = "magi-browser-automation";

export const INSTALL_PAGE_RUNTIME = String.raw`
((runtimeEpoch) => {
  if (globalThis.__magiBrowserAutomation?.runtime_epoch === runtimeEpoch) return;
  const state = {
    runtimeEpoch,
    snapshotRevision: 0,
    nextRef: 1,
    refs: new Map(),
    annotations: [],
    annotationLayer: null,
    annotationShadow: null,
    annotationFrame: 0,
    annotationObserver: null,
    annotationResizeObserver: null,
    annotationMarkers: new Map(),
    annotationListenersInstalled: false,
    pointerGuards: new Map(),
  };
  // iframe 内的元素属于另一个 realm，instanceof Element 对它们恒为 false，
  // 因此所有“是不是元素”的判断都改用 nodeType，保证同源 iframe 内的节点可被快照和操作。
  const isElement = (value) => Boolean(value) && value.nodeType === 1 && typeof value.tagName === 'string';
  const MAX_FRAME_DEPTH = 4;
  const isFrameElement = (element) => {
    const tag = element.tagName?.toLowerCase?.();
    return tag === 'iframe' || tag === 'frame';
  };
  // 只有同源 iframe 才能读到 contentDocument；跨域或已销毁的 frame 返回 null。
  const frameDocument = (element) => {
    if (!isFrameElement(element)) return null;
    try {
      return element.contentDocument || null;
    } catch {
      return null;
    }
  };
  const roleFor = (element) => {
    const explicit = element.getAttribute?.('role');
    if (explicit) return explicit;
    const tag = element.tagName?.toLowerCase?.() || '';
    if (tag === 'iframe' || tag === 'frame') return 'iframe';
    if (tag === 'a' && element.hasAttribute('href')) return 'link';
    if (tag === 'button') return 'button';
    if (tag === 'td' || tag === 'th') return 'cell';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    if (tag === 'img') return 'img';
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (['main', 'nav', 'form', 'section'].includes(tag)) return 'region';
    if (element.draggable) return 'draggable';
    if (tag === 'input') {
      const type = (element.getAttribute('type') || 'text').toLowerCase();
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (['button', 'submit', 'reset'].includes(type)) return 'button';
      return 'textbox';
    }
    return null;
  };
  // 标签与祖先名称不能经 textContent 重新带入 textarea/select 的敏感值。
  const labelText = (root) => {
    if (!root) return '';
    const doc = root.ownerDocument || document;
    const walker = doc.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        if (node.nodeType === 3) return NodeFilter.FILTER_ACCEPT;
        if (sensitiveKind(node) || ['script', 'style', 'template', 'noscript'].includes(node.tagName.toLowerCase())) return NodeFilter.FILTER_REJECT;
        return NodeFilter.FILTER_SKIP;
      },
    });
    if (isElement(root) && sensitiveKind(root)) return '';
    let text = '', visited = 0, node;
    while ((node = walker.nextNode())) {
      text += (node.nodeValue || '').replace(/\s+/g, ' ');
      if (text.length > 240 || ++visited >= 2000) return text.slice(0, 241);
    }
    return text.trim();
  };
  const nameFor = (element) => {
    const tag = element.tagName?.toLowerCase?.() || '';
    const aria = element.getAttribute?.('aria-label')?.trim();
    if (aria) return aria;
    const ownerDoc = element.ownerDocument || document;
    const labelledBy = element.getAttribute?.('aria-labelledby');
    if (labelledBy) {
      const text = labelledBy.split(/\s+/).map((id) => labelText(ownerDoc.getElementById(id))).join(' ').trim();
      if (text) return text;
    }
    const alt = element.getAttribute?.('alt')?.trim();
    if (alt) return alt;
    const title = element.getAttribute?.('title')?.trim();
    if (title) return title;
    const explicitLabel = element.id
      ? labelText(ownerDoc.querySelector('label[for="' + CSS.escape(element.id) + '"]'))
      : '';
    if (explicitLabel) return explicitLabel;
    const parentLabel = labelText(element.closest?.('label'));
    if (parentLabel) return parentLabel;
    if (tag === 'input' && (element.getAttribute('type') || '').toLowerCase() === 'file') return 'file input';
    if (tag === 'iframe' || tag === 'frame') {
      return (element.getAttribute('name') || element.getAttribute('src') || '') || null;
    }
    if (['main', 'nav', 'form', 'section'].includes(tag)) return element.id || tag;
    if (['input', 'textarea', 'select'].includes(tag)) return (element.getAttribute('placeholder') || element.getAttribute('name') || '') || null;
    return labelText(element) || null;
  };
  // iframe 边框及缩放参与坐标换算；旋转/翻转不能用轴对齐矩形安全定位。
  const frameGeometry = (frame) => {
    const rect = frame.getBoundingClientRect();
    return { rect, sx: rect.width / frame.offsetWidth, sy: rect.height / frame.offsetHeight };
  };
  const assertFrameTransform = (frame) => {
    const view = frame.ownerDocument.defaultView;
    for (let node = frame; node; node = node.parentElement || node.getRootNode().host) {
      const style = view.getComputedStyle(node);
      const matrix = new view.DOMMatrixReadOnly(style.transform === 'none' ? undefined : style.transform);
      if (!matrix.is2D || matrix.b !== 0 || matrix.c !== 0 || matrix.a <= 0 || matrix.d <= 0
        || (style.scale !== 'none' && style.scale.split(' ').some(value => Number.parseFloat(value) <= 0))
        || !['none', '0deg'].includes(style.rotate) || style.perspective !== 'none') {
        throw new Error('browser_frame_transform_unsupported');
      }
    }
  };
  // 返回主文档视口坐标，逐层换算同源 iframe 的边框与缩放。
  const rectFor = (element) => {
    const rect = element.getBoundingClientRect();
    let x = rect.x;
    let y = rect.y;
    let width = rect.width, height = rect.height;
    let view = element.ownerDocument?.defaultView;
    for (let depth = 0; view && depth < 16; depth += 1) {
      let frame = null;
      try {
        frame = view.frameElement;
      } catch {
        frame = null;
      }
      if (!frame) break;
      const { rect: frameRect, sx, sy } = frameGeometry(frame);
      x = frameRect.x + (x + frame.clientLeft) * sx;
      y = frameRect.y + (y + frame.clientTop) * sy;
      width *= sx; height *= sy;
      view = frame.ownerDocument?.defaultView;
    }
    return {
      x,
      y,
      width,
      height,
    };
  };
  const visible = (element) => {
    const style = (element.ownerDocument?.defaultView || window).getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== 'none'
      && style.visibility !== 'hidden'
      && Number(style.opacity || '1') > 0
      && rect.width > 0
      && rect.height > 0;
  };
  const sensitiveKind = (element) => {
    const type = (element.getAttribute('type') || '').toLowerCase();
    const autocomplete = (element.getAttribute('autocomplete') || '').toLowerCase();
    if (type === 'password' || autocomplete.includes('password')) return 'password';
    const tokens = autocomplete.split(/\s+/);
    if (tokens.includes('one-time-code')) return 'one_time_code';
    if (tokens.some((token) => token.startsWith('cc-'))) return 'payment_card';
    return null;
  };
  // 元素当前的交互状态（勾选、展开、选中、按下、必填、无效、只读）。模型只能靠这些状态判断
  // “现在是什么样”，取值来自 DOM 属性或 ARIA 属性，在页面内同步读取，不经过 CDP。
  const statesFor = (element) => {
    const states = [];
    const tag = element.tagName?.toLowerCase?.() || '';
    const aria = (name) => element.getAttribute?.(name);
    const type = tag === 'input' ? (aria('type') || 'text').toLowerCase() : '';
    if (type === 'checkbox' || type === 'radio') {
      states.push(element.indeterminate ? 'mixed' : element.checked ? 'checked' : 'unchecked');
    } else if (aria('aria-checked') === 'true') states.push('checked');
    else if (aria('aria-checked') === 'false') states.push('unchecked');
    else if (aria('aria-checked') === 'mixed') states.push('mixed');
    if (tag === 'details') states.push(element.open ? 'expanded' : 'collapsed');
    else if (aria('aria-expanded') === 'true') states.push('expanded');
    else if (aria('aria-expanded') === 'false') states.push('collapsed');
    if (tag === 'option' ? element.selected : aria('aria-selected') === 'true') states.push('selected');
    if (aria('aria-pressed') === 'true') states.push('pressed');
    if (element.required === true || aria('aria-required') === 'true') states.push('required');
    if (aria('aria-invalid') === 'true') states.push('invalid');
    if (element.readOnly === true || aria('aria-readonly') === 'true') states.push('read_only');
    return states;
  };
  const shouldInclude = (element) => {
    if (!isElement(element) || !visible(element)) return false;
    const role = roleFor(element);
    const tag = element.tagName.toLowerCase();
    return Boolean(role)
      || Boolean(element.id && (element.textContent || '').replace(/\s+/g, ' ').trim())
      || ['input', 'textarea', 'select', 'summary', 'details'].includes(tag)
      || element.tabIndex >= 0
      || element.hasAttribute('contenteditable')
      || typeof element.onclick === 'function';
  };
  const refFor = (element) => {
    const ref = 'e:' + state.snapshotRevision + ':' + state.nextRef++;
    state.refs.set(ref, element);
    return ref;
  };
  const snapshotChildren = (root, budget) => {
    const pending = [{ iterator: [root][Symbol.iterator](), depth: 0 }];
    const candidates = [];
    const seen = new Set();
    while (pending.length && budget.scanned < 20000) {
      const current = pending[pending.length - 1];
      const next = current.iterator.next();
      if (next.done) { pending.pop(); continue; }
      const element = next.value, depth = current.depth;
      if (!isElement(element) || seen.has(element)) continue;
      seen.add(element);
      budget.scanned += 1;
      if (shouldInclude(element)) candidates.push(element);
      if (sensitiveKind(element)) continue;
      let children = element.shadowRoot ? element.shadowRoot.children : element.children;
      if (element.tagName.toLowerCase() === 'slot' && element.assignedElements) {
        const assigned = element.assignedElements({ flatten: true });
        if (assigned.length) children = assigned;
      }
      pending.push({ iterator: children[Symbol.iterator](), depth });
      const frame = isFrameElement(element) ? frameDocument(element) : null;
      if (frame?.documentElement) {
        if (depth < MAX_FRAME_DEPTH) pending.push({ iterator: [frame.documentElement][Symbol.iterator](), depth: depth + 1 });
        else budget.truncated = true;
      }
    }
    budget.truncated ||= pending.length > 0;
    const priority = (e) => {
      if ((e.getRootNode().activeElement || e.ownerDocument.activeElement) === e) return 0;
      const role = roleFor(e);
      if (e.isContentEditable || ['textbox', 'searchbox', 'combobox'].includes(role)) return 1;
      if (['button', 'checkbox', 'radio', 'switch'].includes(role)) return 2;
      if (role === 'region' || role === 'heading') return 3;
      if (role === 'link') return 4;
      return 5;
    };
    // 视口内的控件先于视口外的：用户（和 Agent）此刻能看到、能操作的才是预算的第一用途，
    // 文档顺序靠前的页眉、侧边栏历史列表不应挤掉当前屏幕上的控件。
    const onScreen = new Map();
    const isOnScreen = (element) => {
      let value = onScreen.get(element);
      if (value === undefined) {
        const rect = rectFor(element);
        value = rect.x < innerWidth && rect.y < innerHeight && rect.x + rect.width > 0 && rect.y + rect.height > 0;
        onScreen.set(element, value);
      }
      return value;
    };
    const viewportFirst = (list) => {
      const visibleNow = [], elsewhere = [];
      for (const element of list) (isOnScreen(element) ? visibleNow : elsewhere).push(element);
      return visibleNow.concat(elsewhere);
    };
    // 按类型预留位置，避免长表单挤掉所有区域和链接。
    const selectByKind = (list, limits) => {
      const buckets = Array.from({ length: 6 }, () => []);
      for (const element of viewportFirst(list)) buckets[priority(element)].push(element);
      const picked = [];
      for (const [rank, limit] of limits.entries()) picked.push(...buckets[rank].slice(0, limit));
      return picked;
    };
    const selected = [], selectedSet = new Set();
    const addAll = (list) => { for (const element of list) if (!selectedSet.has(element)) { selectedSet.add(element); selected.push(element); } };
    // 模态对话框在最上层：用户（和 Agent）只能操作其中的控件，背景页面是惰性的。
    // 预算必须先给对话框，否则背景里的长列表（表格行、复选框）会挤掉对话框里的表单控件。
    const modal = activeModal();
    if (modal) {
      const inside = candidates.filter(element => containsComposed(modal, element));
      const outside = candidates.filter(element => !containsComposed(modal, element));
      addAll(selectByKind(inside, [8, 40, 60, 40, 40, 20]));
      addAll(viewportFirst(inside));
      addAll(selectByKind(outside, [2, 6, 6, 4, 8, 2]));
      addAll(viewportFirst(outside));
    } else {
      addAll(selectByKind(candidates, [4, 20, 24, 16, 28, 4]));
      addAll(viewportFirst(candidates));
    }
    const encoder = new TextEncoder();
    const encode = (value) => {
      if (!value) return null;
      let result = '';
      for (const ch of value) {
        const size = encoder.encode(ch).length;
        if (budget.textBytes + size > budget.maxTextBytes || result.length + ch.length > 240) { budget.truncated = true; break; }
        result += ch;
        budget.textBytes += size;
      }
      return result || null;
    };
    const nodes = [];
    for (const element of selected) {
      if (nodes.length >= budget.maxNodes || budget.textBytes >= budget.maxTextBytes) { budget.truncated = true; break; }
      const sensitive = sensitiveKind(element);
      const frameAccessible = !isFrameElement(element) || Boolean(frameDocument(element));
      nodes.push({
        element_ref: refFor(element), role: encode(roleFor(element)), name: encode(nameFor(element)),
        value: sensitive ? null : encode(typeof element.value === 'string' ? element.value : null),
        description: encode(element.getAttribute('aria-description') || (element.tagName.toLowerCase() === 'select' && !sensitive
          ? 'options: ' + JSON.stringify([...(element.options || [])].slice(0, 20).map(option => ({ label: option.label || option.textContent, value: option.value, disabled: option.disabled })))
          : element.tagName.toLowerCase() === 'a' && element.href ? 'href: ' + element.href
          : !frameAccessible ? 'iframe 内容不可检查，请用 browser_click_at 按坐标操作' : null)),
        disabled: disabled(element), focused: (element.getRootNode().activeElement || element.ownerDocument.activeElement) === element,
        editable: Boolean(element.isContentEditable) || ['input', 'textarea', 'select'].includes(element.tagName.toLowerCase()),
        sensitive_input_kind: sensitive, states: statesFor(element), visible: true, bounds: rectFor(element), children: [],
      });
    }
    budget.nodes = nodes.length;
    // 没返回的控件按类型和是否在视口内汇总，让 Agent 知道页面上还有什么、是否值得滚动或缩小范围。
    const omitted = selected.slice(nodes.length);
    if (omitted.length) {
      const counts = new Map();
      let inView = 0;
      for (const element of omitted) {
        const key = roleFor(element) || element.tagName.toLowerCase();
        counts.set(key, (counts.get(key) || 0) + 1);
        if (isOnScreen(element)) inView += 1;
      }
      const kinds = [...counts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 6).map(([key, count]) => key + ' ' + count).join('、');
      budget.omitted = '未返回 ' + omitted.length + ' 个控件（视口内 ' + inView + '，视口外 ' + (omitted.length - inView) + '；' + kinds + '）';
    }
    return { nodes, title: encode(document.title) };
  };
  // 当前最上层的模态容器：优先取覆盖视口中心的那个，否则取文档中最后出现的（门户通常追加在 body 末尾）。
  const activeModal = () => {
    const found = [...document.querySelectorAll('dialog[open], [role="dialog"], [role="alertdialog"], [aria-modal="true"]')].filter(visible);
    if (!found.length) return null;
    const center = document.elementFromPoint?.(Math.floor(innerWidth / 2), Math.floor(innerHeight / 2));
    const covering = center ? found.filter(element => containsComposed(element, center)) : [];
    return (covering.length ? covering : found)[(covering.length ? covering : found).length - 1];
  };
  const disabled = (element) => Boolean(element.disabled) || element.matches(':disabled')
    || Boolean(element.closest('[inert], [aria-disabled="true"]'));
  const assertActionable = (element) => {
    if (!element.isConnected) throw new Error('browser_element_ref_stale');
    if (disabled(element)) throw new Error('browser_target_disabled');
    if (!visible(element)) throw new Error('browser_target_not_visible');
  };
  const containsComposed = (ancestor, node) => {
    for (let current = node; current; current = current.parentNode || current.host) if (current === ancestor) return true;
    return false;
  };
  const hitAt = (element, localX, localY) => {
    let root = element.getRootNode();
    let hit = root.elementFromPoint(localX, localY);
    if (!containsComposed(element, hit)) return false;
    // Shadow root 和每一级 iframe 外的遮挡也必须检查。
    while (root.host) {
      const host = root.host;
      root = host.getRootNode();
      hit = root.elementFromPoint(localX, localY);
      if (!containsComposed(host, hit)) return false;
    }
    const view = element.ownerDocument.defaultView;
    if (view?.frameElement) {
      const frame = view.frameElement;
      assertFrameTransform(frame);
      const { rect, sx, sy } = frameGeometry(frame);
      localX = rect.left + (localX + frame.clientLeft) * sx;
      localY = rect.top + (localY + frame.clientTop) * sy;
      return hitAt(frame, localX, localY);
    }
    return true;
  };
  const verifyPoint = (element, x, y) => {
    assertActionable(element);
    if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) throw new Error('browser_pointer_outside_viewport');
    const local = element.getBoundingClientRect(), global = rectFor(element);
    if (!hitAt(element, (x-global.x) * local.width/global.width+local.x, (y-global.y) * local.height/global.height+local.y)) throw new Error('browser_target_obscured');
  };
  const elementAtPoint = (x, y) => {
    if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) throw new Error('browser_pointer_outside_viewport');
    let root = document, element = root.elementFromPoint(x, y);
    while (element) {
      if (isFrameElement(element)) {
        const doc = frameDocument(element);
        // 跨域 frame 保留坐标输入能力；父文档看不到内部事件时不能报告已确认。
        if (!doc) break;
        assertFrameTransform(element);
        const { rect, sx, sy } = frameGeometry(element);
        x = (x-rect.x)/sx-element.clientLeft; y = (y-rect.y)/sy-element.clientTop;
        root = doc;
      } else if (element.shadowRoot) root = element.shadowRoot;
      else break;
      const next = root.elementFromPoint(x, y);
      if (next === element) break;
      element = next;
    }
    if (!element) throw new Error('browser_pointer_target_missing');
    return element;
  };
  const actionPoint = (element) => {
    assertActionable(element);
    const view = element.ownerDocument.defaultView;
    const rect = element.getBoundingClientRect();
    const left = Math.max(0, rect.left), top = Math.max(0, rect.top);
    const right = Math.min(view.innerWidth, rect.right), bottom = Math.min(view.innerHeight, rect.bottom);
    if (right <= left || bottom <= top) throw new Error('browser_target_outside_viewport');
    for (const [fx, fy] of [[.5,.5], [.2,.2], [.8,.2], [.2,.8], [.8,.8]]) {
      const x = left + (right-left)*fx, y = top + (bottom-top)*fy;
      if (hitAt(element, x, y)) {
        const global = rectFor(element);
        return { x: global.x + (x-rect.x) * global.width/rect.width, y: global.y + (y-rect.y) * global.height/rect.height };
      }
    }
    throw new Error('browser_target_obscured');
  };
  const scrollTo = (element) => {
    // 已有可命中的页面位置时保留滚动位置，避免每次点击/输入都把表单滚到中间。
    try { actionPoint(element); return; }
    catch (error) {
      if (!['browser_target_outside_viewport', 'browser_target_obscured'].includes(error.message)) throw error;
    }
    element.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    let view = element.ownerDocument.defaultView;
    while (view?.frameElement) {
      view.frameElement.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      view = view.frameElement.ownerDocument.defaultView;
    }
  };
  const assertTextTarget = (element) => {
    assertActionable(element);
    if (sensitiveKind(element)) throw new Error('browser_sensitive_action_requires_user');
    if (element.readOnly || element.getAttribute('aria-readonly') === 'true') throw new Error('browser_target_read_only');
    const tag = element.tagName.toLowerCase();
    if (!element.isContentEditable && tag !== 'textarea' && !(tag === 'input' && ['text','search','email','url','tel','number'].includes(element.type))) throw new Error('browser_target_not_editable');
  };
  const editableText = (element) => element.isContentEditable
    ? (element.textContent === '' ? '' : element.innerText) : element.value;
  const cssPath = (element) => {
    if (element.id) return '#' + CSS.escape(element.id);
    const parts = [];
    let current = element;
    while (current && current.nodeType === Node.ELEMENT_NODE && parts.length < 8) {
      let part = current.tagName.toLowerCase();
      const testId = current.getAttribute('data-testid');
      if (testId) {
        part += '[data-testid="' + CSS.escape(testId) + '"]';
        parts.unshift(part);
        break;
      }
      const siblings = current.parentElement
        ? [...current.parentElement.children].filter((child) => child.tagName === current.tagName)
        : [];
      if (siblings.length > 1) part += ':nth-of-type(' + (siblings.indexOf(current) + 1) + ')';
      parts.unshift(part);
      current = current.parentElement;
    }
    return parts.join(' > ');
  };
  const fingerprint = (element) => [
    element.tagName?.toLowerCase?.() || '',
    element.id || '',
    element.getAttribute?.('name') || '',
    element.getAttribute?.('role') || '',
    element.getAttribute?.('data-testid') || '',
    nameFor(element) || '',
  ].join('|').slice(0, 512);
  const field = (value, snake, camel) => value?.[snake] ?? value?.[camel];
  const sameAnnotationDocument = (anchor) => {
    const rawUrl = String(field(anchor, 'url', 'url') || '').trim();
    if (!rawUrl) return true;
    try {
      const target = new URL(rawUrl, location.href);
      const current = new URL(location.href);
      return target.origin === current.origin
        && target.pathname === current.pathname
        && target.search === current.search;
    } catch {
      return false;
    }
  };
  const resolveAnnotationElement = (anchor) => {
    const stableId = String(field(anchor, 'stable_id', 'stableId') || '').trim();
    if (stableId) {
      const element = document.getElementById(stableId);
      if (element instanceof Element) return element;
    }
    const testId = String(field(anchor, 'test_id', 'testId') || '').trim();
    if (testId) {
      try {
        const element = document.querySelector('[data-testid="' + CSS.escape(testId) + '"]');
        if (element instanceof Element) return element;
      } catch {}
    }
    const css = String(field(anchor, 'css_path', 'cssPath') || '').trim();
    if (css) {
      try {
        const element = document.querySelector(css);
        if (element instanceof Element) return element;
      } catch {}
    }
    const expected = String(field(anchor, 'dom_fingerprint', 'domFingerprint') || '').trim();
    if (!expected) return null;
    for (const element of document.querySelectorAll('*')) {
      if (fingerprint(element) === expected) return element;
    }
    return null;
  };
  const annotationRect = (annotation) => {
    const anchor = annotation?.anchor || {};
    if (!sameAnnotationDocument(anchor)) return null;
    if (annotation.kind === 'element') {
      const element = resolveAnnotationElement(anchor);
      if (!(element instanceof Element)) return null;
      const rect = rectFor(element);
      return rect.width > 0 && rect.height > 0 ? rect : null;
    }
    if (annotation.kind !== 'region') return null;
    const rect = anchor.rect || {};
    const viewport = anchor.viewport || {};
    const sourceWidth = Number(field(viewport, 'width', 'width')) || innerWidth;
    const sourceHeight = Number(field(viewport, 'height', 'height')) || innerHeight;
    const scrollXAtCapture = Number(field(anchor, 'scroll_x', 'scrollX')) || 0;
    const scrollYAtCapture = Number(field(anchor, 'scroll_y', 'scrollY')) || 0;
    const scaleX = innerWidth / sourceWidth;
    const scaleY = innerHeight / sourceHeight;
    const width = Number(rect.width) * sourceWidth * scaleX;
    const height = Number(rect.height) * sourceHeight * scaleY;
    if (!(width > 0 && height > 0)) return null;
    return {
      x: (Number(rect.x) * sourceWidth + scrollXAtCapture) * scaleX - scrollX,
      y: (Number(rect.y) * sourceHeight + scrollYAtCapture) * scaleY - scrollY,
      width,
      height,
    };
  };
  const ensureAnnotationLayer = () => {
    if (state.annotationLayer?.isConnected && state.annotationShadow) return state.annotationShadow;
    const host = document.createElement('div');
    host.id = 'magi-browser-annotations';
    host.setAttribute('aria-hidden', 'true');
    host.style.cssText = 'position:fixed;inset:0;z-index:2147483646;pointer-events:none;overflow:hidden;';
    const shadow = host.attachShadow({ mode: 'closed' });
    (document.documentElement || document.body)?.append(host);
    state.annotationLayer = host;
    state.annotationShadow = shadow;
    return shadow;
  };
  const annotationStyleText = '.magi-annotation{position:fixed;box-sizing:border-box;border:2px solid #e8590c;background:rgba(255,146,43,.12);border-radius:4px;pointer-events:none}.magi-annotation-badge{position:absolute;left:-2px;top:-2px;display:grid;place-items:center;min-width:20px;height:20px;padding:0 4px;border-radius:10px;background:#e8590c;color:#fff;font:700 12px/20px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;box-shadow:0 1px 3px rgba(0,0,0,.35)}.magi-annotation-label{position:absolute;left:20px;top:-2px;max-width:260px;overflow:hidden;padding:2px 6px;border-radius:3px;background:#e8590c;color:#fff;font:500 11px/16px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;text-overflow:ellipsis;white-space:nowrap;box-shadow:0 1px 3px rgba(0,0,0,.35)}';
  const activeAnnotations = () => state.annotations.filter((annotation) => (
    annotation && (annotation.status === 'active' || annotation.status === 'stale')
  ));
  const annotationKey = (annotation, index) => String(
    annotation.annotation_id
      || annotation.annotationId
      || annotation.sequence
      || index,
  );
  const disconnectAnnotationObservers = () => {
    state.annotationObserver?.disconnect();
    state.annotationResizeObserver?.disconnect();
  };
  const refreshAnnotationObservers = () => {
    disconnectAnnotationObservers();
    const targets = activeAnnotations()
      .map((annotation) => annotation.kind === 'element' ? resolveAnnotationElement(annotation.anchor || {}) : null)
      .filter((element) => element instanceof Element);
    if (!targets.length) return;
    const observed = new Map();
    const addMutationTarget = (target, options) => {
      if (!(target instanceof Element)) return;
      const current = observed.get(target) || { attributes: false, childList: false };
      current.attributes ||= options.attributes === true;
      current.childList ||= options.childList === true;
      observed.set(target, current);
    };
    for (const target of targets) {
      let node = target;
      let depth = 0;
      while (node instanceof Element && depth < 32) {
        addMutationTarget(node, { attributes: true });
        addMutationTarget(node.parentElement, { childList: true });
        node = node.parentElement;
        depth += 1;
      }
    }
    state.annotationObserver = new MutationObserver(() => scheduleAnnotationRender());
    for (const [target, options] of observed) {
      state.annotationObserver.observe(target, {
        attributes: options.attributes,
        attributeFilter: options.attributes ? ['class', 'style', 'hidden', 'open', 'aria-hidden'] : undefined,
        childList: options.childList,
      });
    }
    state.annotationResizeObserver = new ResizeObserver(() => scheduleAnnotationRender());
    for (const target of targets) state.annotationResizeObserver.observe(target);
  };
  const createAnnotationMarker = () => {
    const marker = document.createElement('div');
    marker.className = 'magi-annotation';
    const badge = document.createElement('span');
    badge.className = 'magi-annotation-badge';
    marker.append(badge);
    const label = document.createElement('span');
    label.className = 'magi-annotation-label';
    marker.append(label);
    return { marker, badge, label };
  };
  const renderAnnotations = () => {
    state.annotationFrame = 0;
    const shadow = ensureAnnotationLayer();
    let style = shadow.querySelector('style[data-magi-annotation-style="true"]');
    if (!(style instanceof HTMLStyleElement)) {
      style = document.createElement('style');
      style.dataset.magiAnnotationStyle = 'true';
      style.textContent = annotationStyleText;
      shadow.append(style);
    }
    const visibleKeys = new Set();
    for (const [index, annotation] of activeAnnotations().entries()) {
      const key = annotationKey(annotation, index);
      visibleKeys.add(key);
      const rect = annotationRect(annotation);
      const existing = state.annotationMarkers.get(key) || createAnnotationMarker();
      const marker = existing.marker;
      if (!marker.isConnected) shadow.append(marker);
      const inViewport = rect
        && rect.x <= innerWidth
        && rect.y <= innerHeight
        && rect.x + rect.width >= 0
        && rect.y + rect.height >= 0;
      marker.style.display = inViewport ? 'block' : 'none';
      if (inViewport) {
        marker.style.left = rect.x + 'px';
        marker.style.top = rect.y + 'px';
        marker.style.width = rect.width + 'px';
        marker.style.height = rect.height + 'px';
      }
      const sequence = Number(annotation.sequence) || 0;
      const comment = String(annotation.comment || '').replace(/\s+/g, ' ').trim();
      marker.dataset.annotationId = String(annotation.annotation_id || annotation.annotationId || '');
      marker.setAttribute('aria-label', sequence + '. ' + comment);
      existing.badge.textContent = String(sequence || '•');
      existing.label.textContent = comment.slice(0, 180);
      existing.label.style.display = comment ? 'block' : 'none';
      state.annotationMarkers.set(key, existing);
    }
    for (const [key, existing] of state.annotationMarkers) {
      if (visibleKeys.has(key)) continue;
      existing.marker.remove();
      state.annotationMarkers.delete(key);
    }
    refreshAnnotationObservers();
  };
  const scheduleAnnotationRender = () => {
    if (state.annotationFrame) return;
    state.annotationFrame = requestAnimationFrame(renderAnnotations);
  };
  const installAnnotationObservers = () => {
    if (state.annotationListenersInstalled) return;
    state.annotationListenersInstalled = true;
    addEventListener('scroll', scheduleAnnotationRender, true);
    addEventListener('resize', scheduleAnnotationRender, true);
  };
  globalThis.__magiBrowserAutomation = {
    runtime_epoch: runtimeEpoch,
    // 仅供同一隔离世界的站点适配器使用；沿用节点引用的失效和敏感字段检查。
    reference(element) {
      if (!isElement(element) || !element.isConnected) throw new Error('browser_element_ref_stale');
      return refFor(element);
    },
    viewport() {
      return {
        width: innerWidth,
        height: innerHeight,
        scrollX,
        scrollY,
        deviceScaleFactorMillis: Math.round(devicePixelRatio * 1000),
      };
    },
    snapshot(maxNodes, maxTextBytes, revision, selector) {
      if (!Number.isSafeInteger(maxNodes) || maxNodes < 1 || !Number.isSafeInteger(maxTextBytes) || maxTextBytes < 1) throw new Error('browser_snapshot_limits_invalid');
      if (!Number.isSafeInteger(revision) || revision <= 0) {
        throw new Error('browser_snapshot_revision_invalid');
      }
      // scope 可以是 CSS 选择器，也可以是上一次快照里的 element_ref（缩小到某个弹窗、列表或表单，无需猜选择器）。
      const root = !selector ? (document.body || document.documentElement)
        : /^e:\d+:\d+$/.test(selector) ? state.refs.get(selector)
        : document.querySelector(selector);
      if (!root || !root.isConnected) throw new Error('browser_snapshot_scope_not_found');
      for (const token of state.pointerGuards.keys()) this.finishPointer(token);
      state.snapshotRevision = revision;
      state.nextRef = 1;
      state.refs = new Map();
      const budget = { nodes: 0, scanned: 0, textBytes: 0, maxNodes, maxTextBytes, truncated: false };
      const { nodes: children, title } = snapshotChildren(root, budget);
      return {
        snapshot_revision: state.snapshotRevision,
        root: {
          element_ref: 'root',
          role: 'document',
          name: title,
          value: null,
          description: [
            '视口 ' + innerWidth + '×' + innerHeight + '，已滚动 ' + Math.round(scrollY) + '/' + Math.max(0, Math.round(document.documentElement.scrollHeight - innerHeight)) + 'px',
            budget.omitted || null,
          ].filter(Boolean).join('；'),
          disabled: false,
          focused: false,
          editable: false,
          sensitive_input_kind: null,
          states: [],
          visible: true,
          bounds: { x: 0, y: 0, width: innerWidth, height: innerHeight },
          children,
        },
        returned_nodes: budget.nodes,
        total_nodes: budget.scanned,
        text_bytes: budget.textBytes,
        truncated: budget.truncated,
      };
    },
    // 等页面稳定：文档加载完成且连续 quietMs 没有 DOM 变化；超时仍在变化则如实返回 stable=false。
    // 轮询、动画、实时数据流的页面可能永远不安静，调用方据此直接快照，不应反复等待。
    settle(quietMs, timeoutMs) {
      return new Promise(resolve => {
        const startedAt = Date.now();
        let mutations = 0, lastChangeAt = startedAt, timer = 0;
        const observer = typeof MutationObserver === 'function' ? new MutationObserver(records => { mutations += records.length; lastChangeAt = Date.now(); }) : null;
        observer?.observe(document, { subtree: true, childList: true, attributes: true });
        const finish = stable => {
          clearTimeout(timer);
          observer?.disconnect();
          resolve({ stable, mutations: observer ? mutations : null, waited_ms: Date.now() - startedAt, ready_state: document.readyState });
        };
        const tick = () => {
          const now = Date.now();
          if (document.readyState === 'complete' && now - lastChangeAt >= quietMs) return finish(true);
          if (now - startedAt >= timeoutMs) return finish(false);
          timer = setTimeout(tick, 50);
        };
        tick();
      });
    },
    // 按键没有目标元素，效果证据来自页面：文档变化量，以及焦点元素的状态（焦点、值长度、勾选）。
    // 不观察 characterData：输入的字符通过焦点元素的值长度体现，避免对整篇文档的文本节点订阅。
    beginWatch(token) {
      const describe = () => {
        const element = document.activeElement;
        if (!element || element === document.body) return 'focus=none';
        const parts = ['focus=' + element.tagName.toLowerCase() + (element.id ? '#' + element.id : '')];
        if (!sensitiveKind(element)) {
          if (typeof element.value === 'string') parts.push('value_len=' + element.value.length);
          else if (element.isContentEditable) parts.push('text_len=' + (element.textContent || '').length);
        }
        if (typeof element.checked === 'boolean') parts.push('checked=' + element.checked);
        return parts.join(' ');
      };
      const watch = { describe, mutations: 0, lastMutationAt: 0, before: describe(), observer: null };
      if (typeof MutationObserver === 'function') {
        watch.observer = new MutationObserver(records => { watch.mutations += records.length; watch.lastMutationAt = Date.now(); });
        watch.observer.observe(document, { subtree: true, childList: true, attributes: true });
      }
      (state.watches ||= new Map()).set(token, watch);
    },
    async endWatch(token) {
      const watch = state.watches?.get(token);
      if (!watch) return null;
      const startedAt = Date.now();
      while (watch.observer && Date.now() - startedAt < 400 && Date.now() - Math.max(watch.lastMutationAt, startedAt) < 100) {
        await new Promise(resolve => setTimeout(resolve, 50));
      }
      watch.observer?.disconnect();
      state.watches.delete(token);
      return { mutations: watch.observer ? watch.mutations : null, stateBefore: watch.before, stateAfter: watch.describe() };
    },
    cancelWatch(token) {
      const watch = state.watches?.get(token);
      watch?.observer?.disconnect();
      state.watches?.delete(token);
    },
    // 引用（e:<快照版本>:<序号>）本身标识了所属快照；每次快照都会重建引用表，
    // 所以旧快照、旧文档或 Worker 重启前的引用在这里都查不到，统一视为过期。
    resolve(ref) {
      const element = state.refs.get(ref);
      if (!element || !element.isConnected) throw new Error('browser_element_ref_stale');
      return element;
    },
    target(ref) {
      const element = this.resolve(ref);
      const rect = rectFor(element);
      return {
        x: rect.x + rect.width / 2,
        y: rect.y + rect.height / 2,
        bounds: rect,
        editable: Boolean(element.isContentEditable) || ['input', 'textarea', 'select'].includes(element.tagName.toLowerCase()),
        sensitive: sensitiveKind(element),
        role: roleFor(element),
        name: nameFor(element),
      };
    },
    preparePointer(ref, token, eventType, point, forText = false, forbidSubmitIn = []) {
      const stateSummary = element => {
        const parts = [];
        for (const name of ['aria-expanded', 'aria-checked', 'aria-pressed', 'aria-selected', 'aria-current', 'aria-disabled', 'aria-busy']) {
          const value = element.getAttribute?.(name);
          if (value !== null && value !== undefined) parts.push(name.slice(5) + '=' + value);
        }
        if (typeof element.checked === 'boolean') parts.push('checked=' + element.checked);
        if (element.disabled === true) parts.push('disabled=true');
        if (element.open === true) parts.push('open=true');
        return parts.join(' ');
      };
      const element = ref ? this.resolve(ref) : elementAtPoint(point.x, point.y);
      if (forText) assertTextTarget(element); else assertActionable(element);
      // 结构性拦截：按钮是表单的提交控件，且所在对话框/表单的文字涉及受保护的资源（如密钥）时拒绝点击。
      // 判断依据是 DOM 语义（提交按钮 + 所在表单上下文），不依赖按钮自身的文案或语言。
      if (Array.isArray(forbidSubmitIn) && forbidSubmitIn.length) {
        const control = element.closest?.('button, input, [role="button"]') || element;
        const typeAttr = control.getAttribute?.('type');
        // 现代页面的对话框确认按钮常常不在 <form> 里（点击处理函数提交）：对话框内标着保存/创建/确认的按钮同样算提交。
        const label = String(control.getAttribute?.('aria-label') || control.textContent || '').trim().toLowerCase();
        const confirmLike = Boolean(control.closest?.('[role="dialog"], dialog'))
          && ['save', 'create', 'submit', 'confirm', 'apply', 'delete', 'remove', '保存', '创建', '提交', '确认', '应用', '删除'].some(prefix => label.startsWith(prefix));
        const submitLike = (control.tagName === 'BUTTON' && control.form && (typeAttr === null || typeAttr === 'submit'))
          || (control.tagName === 'INPUT' && typeAttr === 'submit') || typeAttr === 'submit' || confirmLike;
        if (submitLike) {
          const scope = control.closest?.('[role="dialog"], dialog, form') || control.form;
          const context = String(scope?.innerText || scope?.textContent || '').toLowerCase();
          if (forbidSubmitIn.some(word => context.includes(String(word).toLowerCase()))) throw new Error('browser_submit_forbidden');
        }
      }
      if (!point) scrollTo(element);
      const position = point || actionPoint(element);
      verifyPoint(element, position.x, position.y);
      const guard = { element, eventType, position, bounds: rectFor(element), observed: false, listeners: [], stateSummary,
        stateBefore: stateSummary(element), mutations: 0, lastMutationAt: 0, observer: null };
      // 动作结果的页面证据：目标自身状态、文档变化量和目标是否仍在文档中。
      // 为输入框获得焦点的点击不收集页面变化：输入前不能有额外的等待窗口，焦点必须原样交给紧随其后的输入。
      if (!forText && typeof MutationObserver === 'function') {
        guard.observer = new MutationObserver(records => { guard.mutations += records.length; guard.lastMutationAt = Date.now(); });
        guard.observer.observe(element.ownerDocument, { subtree: true, childList: true, attributes: true, characterData: true });
      }
      const listener = event => {
        if (isFrameElement(element) && !frameDocument(element)) return;
        if (event.isTrusted && event.composedPath().includes(element)) {
          guard.observed = true;
          guard.complete?.();
        }
      };
      // 自定义 listbox 选项常在 pointerup 完成选择并卸载节点，此后不会再有 click。
      // 仅对 option 接受命中该节点的可信 pointerup；普通按钮仍须确认 click。
      const observedEvents = eventType === 'click' && roleFor(element) === 'option'
        ? ['click', 'pointerup'] : [eventType];
      for (const type of observedEvents) {
        element.ownerDocument.addEventListener(type, listener, true);
        guard.listeners.push([element.ownerDocument, type, listener]);
      }
      state.pointerGuards.set(String(token), guard);
      const rect = rectFor(element);
      return { x: position.x, y: position.y, bounds: rect, role: roleFor(element), name: nameFor(element) };
    },
    verifyPointer(token, x, y) {
      const guard = state.pointerGuards.get(String(token));
      if (!guard) throw new Error('browser_pointer_observation_missing');
      verifyPoint(guard.element, x, y);
      return true;
    },
    relocatePointer(token, x, y) {
      const guard = state.pointerGuards.get(String(token));
      if (!guard) throw new Error('browser_pointer_observation_missing');
      try { verifyPoint(guard.element, x, y); return null; }
      catch (error) {
        if (error.message !== 'browser_target_obscured') throw error;
        const bounds = rectFor(guard.element);
        // 真正的遮挡仍然拒绝；只在同一节点确实重排且尚未按下时重取命中点。
        if (['x', 'y', 'width', 'height'].every(key => Math.abs(bounds[key] - guard.bounds[key]) < .5)) throw error;
        const position = actionPoint(guard.element);
        guard.bounds = bounds;
        return { ...position, bounds, role: roleFor(guard.element), name: nameFor(guard.element) };
      }
    },
    async finishPointer(token, waitForEvent = false) {
      const guard = state.pointerGuards.get(String(token));
      if (!guard) return { observed: false };
      // CDP 的命令回包不是 DOM 事件交付屏障。只在事件尚未到达时等可信事件，
      // 已确认的动作立即结束；离屏页不依赖会被 Chromium 限流的动画帧。
      if (waitForEvent && !guard.observed) {
        await new Promise(resolve => {
          const timer = setTimeout(() => { guard.complete = null; resolve(); }, 250);
          guard.complete = () => { clearTimeout(timer); guard.complete = null; resolve(); };
        });
      }
      // 事件已送达后等页面“安静”一小段：点击的效果（展开、勾选、弹层、列表刷新）常在事件处理后异步出现。
      if (guard.observer && guard.observed && guard.eventType !== 'mousemove') {
        const startedAt = Date.now();
        while (Date.now() - startedAt < 400 && Date.now() - Math.max(guard.lastMutationAt, startedAt) < 100) {
          await new Promise(resolve => setTimeout(resolve, 50));
        }
      }
      guard.observer?.disconnect();
      for (const [doc, type, listener] of guard.listeners) doc.removeEventListener(type, listener, true);
      state.pointerGuards.delete(String(token));
      guard.complete?.();
      // 经过目标的 mousemove 不代表结束时仍在悬停：页面可在处理器中
      // 移走或遮住目标。只回读当前命中，不补移、不触发额外点击。
      if (waitForEvent && guard.eventType === 'mousemove' && guard.observed) verifyPoint(guard.element, guard.position.x, guard.position.y);
      if (!guard.element) return { observed: guard.observed };
      const connected = guard.element.isConnected !== false;
      return {
        observed: guard.observed,
        mutations: Math.min(guard.mutations, 99),
        connected,
        stateBefore: guard.stateBefore,
        stateAfter: connected && guard.stateSummary ? guard.stateSummary(guard.element) : '',
      };
    },
    prepareDrag(sourceRef, targetRef, token) {
      const source = this.resolve(sourceRef), target = this.resolve(targetRef);
      assertActionable(source); assertActionable(target);
      if (source === target) throw new Error('browser_drag_same_target');
      if (!source.draggable) throw new Error('browser_drag_source_not_draggable');
      scrollTo(target); scrollTo(source);
      // 在投递输入之前确认两端均有有效命中点；不在按住鼠标后猜测页面坐标。
      const start = actionPoint(source), end = actionPoint(target);
      const guard = { source, target, observed: false, listeners: [] };
      const listener = event => {
        if (event.isTrusted && event.composedPath().includes(target)) guard.observed = true;
      };
      target.ownerDocument.addEventListener('drop', listener, true);
      guard.listeners.push([target.ownerDocument, 'drop', listener]);
      state.pointerGuards.set(String(token), guard);
      return { source: start, target: end };
    },
    verifyDrag(token) {
      const guard = state.pointerGuards.get(String(token));
      if (!guard) throw new Error('browser_pointer_observation_missing');
      return { source: actionPoint(guard.source), target: actionPoint(guard.target) };
    },
    selectOptions(ref, values) {
      const element = this.resolve(ref);
      this.formControl(ref);
      if (element.tagName !== 'SELECT') throw new Error('browser_fill_form_target_stale');
      if (!element.multiple && values.length !== 1) throw new Error('browser_fill_form_invalid');
      const options = [...element.options];
      const selected = values.map(value => {
        const matches = options.filter(option => option.value === value);
        if (!matches.length) throw new Error('browser_fill_form_select_option_not_found');
        const option = matches.find(option => !disabled(option));
        if (!option) throw new Error('browser_fill_form_select_option_disabled');
        return option;
      });
      // 全部选项验证通过才写入，禁止先改一部分再发现禁用选项。
      try {
        for (const option of options) option.selected = selected.includes(option);
        const EventCtor = element.ownerDocument.defaultView.Event;
        element.dispatchEvent(new EventCtor('input', { bubbles: true }));
        element.dispatchEvent(new EventCtor('change', { bubbles: true }));
        const actual = [...element.selectedOptions];
        return { applied: element.isConnected && actual.length === selected.length && actual.every(option => selected.includes(option)) };
      } catch {
        return { applied: false };
      }
    },
    prepareText(ref, text, replace) {
      const element = this.resolve(ref);
      assertTextTarget(element);
      this.verifyTextFocus(ref);
      const before = editableText(element);
      if (typeof before !== 'string') throw new Error('browser_input_unverifiable');
      if (before.length + text.length > 1000000) throw new Error('browser_input_too_large');
      return { target: this.target(ref), expected: (replace ? '' : before) + text };
    },
    verifyTextFocus(ref) {
      const element = this.resolve(ref);
      assertTextTarget(element);
      if (element.getRootNode().activeElement !== element || !element.ownerDocument.hasFocus()) throw new Error('browser_target_not_focused');
      return true;
    },
    formControl(ref) {
      const element = this.resolve(ref);
      assertActionable(element);
      if (sensitiveKind(element)) throw new Error('browser_sensitive_action_requires_user');
      if (element.tagName === 'SELECT') return { kind: 'select', multiple: element.multiple };
      if (element.tagName === 'INPUT' && ['checkbox', 'radio'].includes(element.type)) return { kind: element.type };
      return { kind: 'text' };
    },
    verifyText(ref, expected) {
      const element = this.resolve(ref);
      if (sensitiveKind(element)) throw new Error('browser_sensitive_action_requires_user');
      const value = editableText(element);
      if (typeof value !== 'string') throw new Error('browser_input_unverifiable');
      return { applied: value === expected };
    },
    hitTest(normalizedX, normalizedY) {
      if (!Number.isFinite(normalizedX) || !Number.isFinite(normalizedY)
        || normalizedX < 0 || normalizedX > 1 || normalizedY < 0 || normalizedY > 1) {
        throw new Error('browser_hit_test_coordinates_invalid');
      }
      if (innerWidth <= 0 || innerHeight <= 0) throw new Error('browser_viewport_unavailable');
      // 标记选择使用内容槽归一化坐标；只有页面运行时知道 Chromium 的
      // CSS 视口尺寸，因此在这里完成唯一一次坐标转换。
      const x = Math.min(innerWidth - 1, normalizedX * innerWidth);
      const y = Math.min(innerHeight - 1, normalizedY * innerHeight);
      const element = document.elementFromPoint(Math.max(0, x), Math.max(0, y));
      if (!(element instanceof Element)) throw new Error('browser_hit_test_empty');
      const ref = refFor(element);
      const rect = rectFor(element);
      return {
        navigation_revision: 0,
        viewport_width: innerWidth,
        viewport_height: innerHeight,
        device_scale_factor_millis: Math.round(devicePixelRatio * 1000),
        scroll_x: scrollX,
        scroll_y: scrollY,
        element_ref: ref,
        tag_name: element.tagName.toLowerCase(),
        test_id: element.getAttribute('data-testid'),
        stable_id: element.id || null,
        aria_role: roleFor(element),
        aria_name: nameFor(element),
        text_excerpt: (element.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 240) || null,
        css_path: cssPath(element),
        ancestor_fingerprint: fingerprint(element.parentElement || element),
        dom_fingerprint: fingerprint(element),
        bounds: rect,
      };
    },
    query(selector) {
      const element = document.querySelector(selector);
      if (!(element instanceof Element)) return null;
      return { ref: refFor(element), bounds: rectFor(element), text: nameFor(element) };
    },
    cssPath(element) {
      return cssPath(element);
    },
    // 读取页面正文：按渲染结构输出文本，穿透 open shadow DOM 和同源 iframe，
    // 跳过脚本、样式和不可见节点，不返回密码等敏感输入的值。
    readText(ref, options) {
      const opts = options || {};
      const maxChars = Math.max(1, Math.min(Number(opts.maxChars) || 12000, 50000));
      const offset = Math.max(0, Number(opts.offset) || 0);
      const includeLinks = opts.includeLinks === true;
      const query = typeof opts.query === 'string' ? opts.query.trim() : '';
      const scope = ref && ref !== 'root' ? this.resolve(ref) : (document.body || document.documentElement);
      const SKIP_TAGS = new Set(['script', 'style', 'noscript', 'template', 'head', 'svg', 'canvas', 'link', 'meta']);
      const MAX_SCANNED_ELEMENTS = 20000;
      const frames = [];
      let scanned = 0;
      let scanTruncated = false;
      let out = '';
      const endsWithBreak = () => out === '' || out.endsWith('\n');
      const breakLine = () => {
        if (!endsWithBreak()) out += '\n';
      };
      const appendInline = (text) => {
        let value = text.replace(/\s+/g, ' ');
        if (!value.trim()) {
          if (!endsWithBreak() && !out.endsWith(' ')) out += ' ';
          return;
        }
        if (endsWithBreak() || out.endsWith(' ')) value = value.replace(/^ /, '');
        out += value;
      };
      const childNodesOf = (node) => {
        const tag = node.tagName.toLowerCase();
        if (tag === 'slot' && typeof node.assignedNodes === 'function') {
          const assigned = node.assignedNodes({ flatten: true });
          if (assigned.length > 0) return assigned;
        }
        return node.shadowRoot ? node.shadowRoot.childNodes : node.childNodes;
      };
      const walk = (node, frameDepth) => {
        if (scanned >= MAX_SCANNED_ELEMENTS) {
          scanTruncated = true;
          return;
        }
        if (node.nodeType === 3) {
          appendInline(node.nodeValue || '');
          return;
        }
        if (!isElement(node)) return;
        scanned += 1;
        const tag = node.tagName.toLowerCase();
        if (SKIP_TAGS.has(tag)) return;
        if (sensitiveKind(node)) return;
        const view = node.ownerDocument?.defaultView || window;
        const style = view.getComputedStyle(node);
        if (style.display === 'none' || style.visibility === 'hidden') return;
        if (isFrameElement(node)) {
          const label = (node.getAttribute('title') || node.getAttribute('name') || node.getAttribute('src') || '').slice(0, 120);
          const frameDoc = frameDepth < MAX_FRAME_DEPTH ? frameDocument(node) : null;
          if (frames.length < 20) frames.push({ src: (node.getAttribute('src') || '').slice(0, 300), accessible: Boolean(frameDoc) });
          breakLine();
          if (!frameDoc) {
            out += '[iframe 内容不可读取（跨域或超过嵌套深度）: ' + label + ']\n';
            return;
          }
          out += '[iframe: ' + label + ']\n';
          const frameRoot = frameDoc.body || frameDoc.documentElement;
          if (frameRoot) walk(frameRoot, frameDepth + 1);
          breakLine();
          return;
        }
        if (tag === 'br') {
          out += '\n';
          return;
        }
        if (tag === 'hr') {
          breakLine();
          out += '---\n';
          return;
        }
        if (tag === 'input') {
          const type = (node.getAttribute('type') || 'text').toLowerCase();
          if (type === 'hidden') return;
          if (type === 'checkbox' || type === 'radio') {
            out += node.checked ? '[x] ' : '[ ] ';
          } else if (type !== 'button' && type !== 'submit' && type !== 'reset' && type !== 'image' && type !== 'file') {
            const value = node.value || '';
            if (value) appendInline('[' + value + ']');
          }
          return;
        }
        if (tag === 'textarea') {
          const value = node.value || '';
          if (value) {
            breakLine();
            out += value + '\n';
          }
          return;
        }
        if (tag === 'select') {
          const chosen = [...node.selectedOptions].map((option) => (option.textContent || '').replace(/\s+/g, ' ').trim()).filter(Boolean);
          if (chosen.length) appendInline('[' + chosen.join(', ') + ']');
          return;
        }
        const display = style.display;
        const isBlock = !(display.startsWith('inline') || display === 'contents');
        const isCell = tag === 'td' || tag === 'th';
        if (isBlock && !isCell) breakLine();
        if (/^h[1-6]$/.test(tag)) out += '#'.repeat(Number(tag[1])) + ' ';
        else if (tag === 'li') out += '- ';
        else if (tag === 'pre' || tag === 'code') { /* 保留原样由子文本节点输出 */ }
        const linkStart = out.length;
        for (const child of childNodesOf(node)) {
          walk(child, frameDepth);
          if (scanned >= MAX_SCANNED_ELEMENTS) break;
        }
        if (includeLinks && tag === 'a' && node.href && out.length > linkStart && /^https?:/i.test(node.href)) {
          out += ' (' + node.href + ')';
        }
        if (isCell) out += ' | ';
        if (isBlock && !isCell) breakLine();
      };
      walk(scope, 0);
      const full = out.replace(/[ \t]+\n/g, '\n').replace(/\n{3,}/g, '\n\n').trim();
      const base = { url: location.href, title: document.title || '', total_chars: full.length, scan_truncated: scanTruncated, frames };
      if (query) {
        const needle = query.toLowerCase();
        const haystack = full.toLowerCase();
        const matches = [];
        let from = 0;
        let total = 0;
        while (true) {
          const index = haystack.indexOf(needle, from);
          if (index < 0) break;
          total += 1;
          if (matches.length < 50) {
            matches.push({
              index,
              context: full.slice(Math.max(0, index - 80), Math.min(full.length, index + needle.length + 80)).replace(/\s+/g, ' '),
            });
          }
          from = index + Math.max(1, needle.length);
        }
        return { ...base, query, match_count: total, matches };
      }
      const text = full.slice(offset, offset + maxChars);
      const end = offset + text.length;
      return { ...base, offset, returned_chars: text.length, truncated: end < full.length, next_offset: end < full.length ? end : null, text };
    },
    // 当前页面所在源的 localStorage / sessionStorage 读写。列表里的值截断，避免单次结果过大。
    storage(area, action, key, value) {
      const store = area === 'session' ? sessionStorage : localStorage;
      const base = { origin: location.origin, area };
      const LIST_VALUE_LIMIT = 2000;
      const GET_VALUE_LIMIT = 50000;
      if (action === 'list') {
        const entries = [];
        for (let index = 0; index < store.length && entries.length < 200; index += 1) {
          const entryKey = store.key(index);
          if (entryKey === null) continue;
          const entryValue = store.getItem(entryKey) ?? '';
          entries.push({
            key: entryKey,
            value: entryValue.slice(0, LIST_VALUE_LIMIT),
            length: entryValue.length,
            truncated: entryValue.length > LIST_VALUE_LIMIT,
          });
        }
        return { ...base, total: store.length, entries };
      }
      if (action === 'get') {
        const entryValue = store.getItem(key);
        return entryValue === null
          ? { ...base, key, found: false }
          : { ...base, key, found: true, value: entryValue.slice(0, GET_VALUE_LIMIT), length: entryValue.length, truncated: entryValue.length > GET_VALUE_LIMIT };
      }
      if (action === 'set') {
        store.setItem(key, String(value));
        return { ...base, key, length: String(value).length };
      }
      if (action === 'remove') {
        const existed = store.getItem(key) !== null;
        store.removeItem(key);
        return { ...base, key, removed: existed };
      }
      if (action === 'clear') {
        const cleared = store.length;
        store.clear();
        return { ...base, cleared };
      }
      throw new Error('browser_storage_action_invalid');
    },
    evaluate(expression) {
      return (0, eval)(expression);
    },
    setAnnotations(annotations) {
      state.annotations = Array.isArray(annotations) ? annotations : [];
      ensureAnnotationLayer();
      installAnnotationObservers();
      renderAnnotations();
      return { rendered: state.annotations.length };
    },
  };
})
`;
