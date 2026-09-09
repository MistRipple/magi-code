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
    clickGuards: new Map(),
  };
  const roleFor = (element) => {
    const explicit = element.getAttribute?.('role');
    if (explicit) return explicit;
    const tag = element.tagName?.toLowerCase?.() || '';
    if (tag === 'a' && element.hasAttribute('href')) return 'link';
    if (tag === 'button') return 'button';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    if (tag === 'img') return 'img';
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
  const nameFor = (element) => {
    const tag = element.tagName?.toLowerCase?.() || '';
    const aria = element.getAttribute?.('aria-label')?.trim();
    if (aria) return aria;
    const labelledBy = element.getAttribute?.('aria-labelledby');
    if (labelledBy) {
      const text = labelledBy.split(/\s+/).map((id) => document.getElementById(id)?.textContent || '').join(' ').trim();
      if (text) return text;
    }
    const alt = element.getAttribute?.('alt')?.trim();
    if (alt) return alt;
    const title = element.getAttribute?.('title')?.trim();
    if (title) return title;
    const explicitLabel = element.id
      ? document.querySelector('label[for="' + CSS.escape(element.id) + '"]')?.textContent?.replace(/\s+/g, ' ').trim()
      : '';
    if (explicitLabel) return explicitLabel;
    const parentLabel = element.closest?.('label')?.textContent?.replace(/\s+/g, ' ').trim();
    if (parentLabel) return parentLabel;
    if (tag === 'input' && (element.getAttribute('type') || '').toLowerCase() === 'file') return 'file input';
    const text = (element.innerText || element.textContent || '').replace(/\s+/g, ' ').trim();
    return text.slice(0, 240) || null;
  };
  const rectFor = (element) => {
    const rect = element.getBoundingClientRect();
    return {
      x: rect.x,
      y: rect.y,
      width: rect.width,
      height: rect.height,
    };
  };
  const visible = (element) => {
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== 'none'
      && style.visibility !== 'hidden'
      && Number(style.opacity || '1') > 0
      && rect.width > 0
      && rect.height > 0;
  };
  const sensitiveKind = (element) => {
    if (element.tagName?.toLowerCase?.() !== 'input') return null;
    const type = (element.getAttribute('type') || '').toLowerCase();
    const autocomplete = (element.getAttribute('autocomplete') || '').toLowerCase();
    if (type === 'password' || autocomplete.includes('password')) return 'password';
    if (autocomplete === 'one-time-code') return 'one_time_code';
    if (autocomplete.startsWith('cc-')) return 'payment_card';
    return null;
  };
  const shouldInclude = (element) => {
    if (!(element instanceof Element) || !visible(element)) return false;
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
  const serialize = (element, budget) => {
    if (budget.nodes >= budget.maxNodes) return null;
    const include = shouldInclude(element);
    const children = [];
    const childRoots = element.shadowRoot
      ? [...element.children, ...element.shadowRoot.children]
      : [...element.children];
    for (const child of childRoots) {
      const serialized = serialize(child, budget);
      if (serialized) children.push(serialized);
      if (budget.nodes >= budget.maxNodes) break;
    }
    if (!include && children.length === 0) return null;
    const name = include ? nameFor(element) : null;
    if (name) budget.textBytes += new TextEncoder().encode(name).byteLength;
    if (budget.textBytes > budget.maxTextBytes) return null;
    budget.nodes += 1;
    const elementRef = include
      ? refFor(element)
      : 'group:' + state.snapshotRevision + ':' + state.nextRef++;
    // 结构性元素（例如没有 ARIA role 的 draggable div）也可能是浏览器
    // 交互的真实目标。快照此前只返回 group 引用但没有登记到 refs，导致
    // 模型按可见名称选择拖拽源/目标后必然得到 browser_element_ref_stale。
    // group 仍保持“非交互节点”的语义，但在当前快照内允许运行时解析到
    // 它对应的 DOM 元素；动作能力仍由各工具自身校验。
    state.refs.set(elementRef, element);
    return {
      element_ref: elementRef,
      role: roleFor(element),
      name,
      value: 'value' in element && typeof element.value === 'string' ? element.value.slice(0, 240) : null,
      description: element.getAttribute?.('aria-description') || null,
      disabled: Boolean(element.disabled) || element.getAttribute?.('aria-disabled') === 'true',
      focused: document.activeElement === element,
      editable: Boolean(element.isContentEditable) || ['input', 'textarea', 'select'].includes(element.tagName.toLowerCase()),
      sensitive_input_kind: sensitiveKind(element),
      visible: true,
      bounds: include ? rectFor(element) : null,
      children,
    };
  };
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
    const width = Number(rect.width) * sourceWidth;
    const height = Number(rect.height) * sourceHeight;
    if (!(width > 0 && height > 0)) return null;
    return {
      x: Number(rect.x) * sourceWidth + scrollXAtCapture - scrollX,
      y: Number(rect.y) * sourceHeight + scrollYAtCapture - scrollY,
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
    viewport() {
      return { width: innerWidth, height: innerHeight };
    },
    snapshot(maxNodes, maxTextBytes, revision) {
      if (!Number.isSafeInteger(revision) || revision <= 0) {
        throw new Error('browser_snapshot_revision_invalid');
      }
      state.snapshotRevision = revision;
      state.nextRef = 1;
      state.refs = new Map();
      const budget = { nodes: 0, textBytes: 0, maxNodes, maxTextBytes };
      const children = [];
      const root = document.body || document.documentElement;
      if (root) {
        const serialized = serialize(root, budget);
        if (serialized) children.push(serialized);
      }
      return {
        snapshot_revision: state.snapshotRevision,
        root: {
          element_ref: 'root',
          role: 'document',
          name: document.title || null,
          value: null,
          description: null,
          disabled: false,
          focused: false,
          editable: false,
          sensitive_input_kind: null,
          visible: true,
          bounds: { x: 0, y: 0, width: innerWidth, height: innerHeight },
          children,
        },
        returned_nodes: budget.nodes,
        total_nodes: document.querySelectorAll('*').length,
        text_bytes: budget.textBytes,
        truncated: budget.nodes >= maxNodes || budget.textBytes >= maxTextBytes,
      };
    },
    resolve(ref, revision) {
      if (revision !== state.snapshotRevision) throw new Error('browser_element_ref_stale');
      const element = state.refs.get(ref);
      if (!element || !element.isConnected) throw new Error('browser_element_ref_stale');
      return element;
    },
    target(ref, revision) {
      const element = this.resolve(ref, revision);
      const rect = rectFor(element);
      return {
        x: rect.x + rect.width / 2,
        y: rect.y + rect.height / 2,
        bounds: rect,
        editable: Boolean(element.isContentEditable) || ['input', 'textarea', 'select'].includes(element.tagName.toLowerCase()),
        sensitive: sensitiveKind(element),
      };
    },
    focus(ref, revision) {
      const element = this.resolve(ref, revision);
      element.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      element.focus({ preventScroll: true });
      return this.target(ref, revision);
    },
    prepareClick(ref, revision, token) {
      const element = this.resolve(ref, revision);
      const guard = { element, observed: false, listener: null };
      guard.listener = () => { guard.observed = true; };
      element.addEventListener('click', guard.listener, { capture: true, once: true });
      state.clickGuards.set(String(token), guard);
      setTimeout(() => {
        if (state.clickGuards.get(String(token)) !== guard) return;
        element.removeEventListener('click', guard.listener, true);
        state.clickGuards.delete(String(token));
      }, 60000);
      return { prepared: true };
    },
    finishClick(token) {
      const guard = state.clickGuards.get(String(token));
      if (!guard) return { observed: false };
      guard.element.removeEventListener('click', guard.listener, true);
      state.clickGuards.delete(String(token));
      return { observed: guard.observed };
    },
    fallbackClick(ref, revision) {
      const element = this.resolve(ref, revision);
      // Runtime.evaluate 必须先返回，避免 alert/confirm/prompt 在同步
      // evaluate 内阻塞 CDP。下一轮任务会通过 Page.javascriptDialogOpening
      // 收到对话框事件，并可由 browser_dialog 接管。
      setTimeout(() => {
        if (element.isConnected) element.click();
      }, 0);
      return { scheduled: true };
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
    elementRef(element) {
      return element instanceof Element && element.isConnected ? refFor(element) : null;
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
