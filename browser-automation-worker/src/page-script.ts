export const MAGI_AUTOMATION_WORLD = "magi-browser-automation";

export const INSTALL_PAGE_RUNTIME = String.raw`
((runtimeEpoch) => {
  if (globalThis.__magiBrowserAutomation?.runtime_epoch === runtimeEpoch) return;
  const state = {
    runtimeEpoch,
    snapshotRevision: 0,
    nextRef: 1,
    refs: new Map(),
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
    return {
      element_ref: include ? refFor(element) : 'group:' + state.snapshotRevision + ':' + state.nextRef++,
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
  globalThis.__magiBrowserAutomation = {
    runtime_epoch: runtimeEpoch,
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
    hitTest(x, y) {
      const element = document.elementFromPoint(x, y);
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
    evaluate(expression) {
      return (0, eval)(expression);
    },
  };
})
`;
