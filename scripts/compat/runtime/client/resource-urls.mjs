// 浏览器资源地址属于宿主命名空间；资源字节、版本和执行仍由 daemon 的原路由拥有。
export function createResourceUrls(baseUri, resourceBase, staticFiles = []) {
  const base = new URL(baseUri);
  const gateway = new URL(resourceBase, base);
  if (gateway.origin !== base.origin || !gateway.pathname.endsWith('/')) throw new Error('插件资源入口无效');
  const staticUrls = new Set(staticFiles.map(file => new URL(file, base).href));
  const directory = new URL('.', base).pathname;
  const restore = input => {
    const url = new URL(input, base);
    if (url.origin === gateway.origin && url.pathname.startsWith(gateway.pathname)) {
      url.pathname = '/' + url.pathname.slice(gateway.pathname.length);
    } else if (url.origin === base.origin && url.pathname.startsWith(directory)) {
      // 原包也会以 document.baseURI 构造业务 URL；移除 Magi 生成的文档目录，
      // 与资源入口恢复到同一原包路由命名空间，不把 ui-v/... 送给后台。
      url.pathname = '/' + url.pathname.slice(directory.length);
    }
    return url;
  };
  return {
    restore,
    resolve(input) {
      const value = String(input);
      if (!value || value.startsWith('#')) return value;
      let url;
      try { url = new URL(value, base); }
      catch { return value; } // 无效地址仍交给浏览器触发原生 error，而非改变页面的同步执行流程。
      if (url.origin !== base.origin || !['http:', 'https:'].includes(url.protocol)
        || staticUrls.has(url.href) || url.pathname.startsWith(gateway.pathname)) return value;
      let path = url.pathname;
      if (path.startsWith(directory)) path = '/' + path.slice(directory.length);
      return gateway.origin + gateway.pathname + path.slice(1) + url.search + url.hash;
    },
  };
}

// 按 HTML srcset 的 URL／描述符边界处理候选项，不用逗号切开 data URL，
// 保留空白、描述符与无效项，密度／宽度选择和错误仍完全由浏览器决定。
export function resolveSrcset(input, resolve) {
  const value = String(input);
  const space = character => /[\t\n\f\r ]/.test(character ?? '');
  let offset = 0, copied = 0, result = '';
  while (offset < value.length) {
    while (offset < value.length && (space(value[offset]) || value[offset] === ',')) offset++;
    const start = offset;
    while (offset < value.length && !space(value[offset])) offset++;
    let end = offset;
    while (value[end - 1] === ',') end--;
    if (end > start) {
      result += value.slice(copied, start) + resolve(value.slice(start, end));
      copied = end;
    }
    if (end < offset) continue;
    let parentheses = 0;
    while (offset < value.length) {
      const character = value[offset++];
      if (character === '(') parentheses++;
      else if (character === ')' && parentheses) parentheses--;
      else if (character === ',' && !parentheses) break;
    }
  }
  return result + value.slice(copied);
}

// 保留原生 <script>／<link>／<img> 的加载、CORS、模块相对导入与错误事件。
// 只转换其网络地址，不读取或改写页面组件、脚本正文、样式或图片。
export function installResourceUrls(window, urls) {
  const html = 'http://www.w3.org/1999/xhtml', svg = 'http://www.w3.org/2000/svg';
  // 同一张表同时驱动 DOM 属性、普通属性及带命名空间属性，避免各自漏接图片入口。
  const elements = [
    [html, 'script', { src: 'src' }], [html, 'link', { href: 'href', imagesrcset: 'imageSrcset' }],
    [html, 'img', { src: 'src', srcset: 'srcset' }], [html, 'source', { src: 'src', srcset: 'srcset' }],
    [html, 'video', { src: 'src', poster: 'poster' }], [html, 'audio', { src: 'src' }],
    [svg, 'image', { href: 'href' }], [svg, 'feImage', { href: 'href' }],
  ];
  const attributes = new Map(elements.map(([namespace, tag, names]) => [namespace + ':' + tag, names]));
  const resolve = (name, value) => name === 'srcset' || name === 'imagesrcset'
    ? resolveSrcset(value, input => urls.resolve(input)) : urls.resolve(value);
  for (const [namespace, tag, names] of elements) {
    const prototype = window.document.createElementNS(namespace, tag).constructor.prototype;
    for (const [attribute, property] of Object.entries(names)) {
      let owner = prototype, descriptor;
      while (owner && !(descriptor = Object.getOwnPropertyDescriptor(owner, property))) owner = Object.getPrototypeOf(owner);
      if (!descriptor?.get || !descriptor.set) continue;
      Object.defineProperty(prototype, property, {
        ...descriptor,
        set(value) { descriptor.set.call(this, resolve(attribute, value)); },
      });
    }
  }
  const mapAttribute = (element, namespace, name, value) => {
    const local = String(name).split(':').at(-1);
    const attribute = element.namespaceURI === html ? local.toLowerCase() : local;
    const names = attributes.get(element.namespaceURI + ':' + element.localName);
    return (!namespace || namespace === 'http://www.w3.org/1999/xlink') && Object.hasOwn(names ?? {}, attribute)
      ? resolve(attribute, value) : value;
  };
  for (const [method, hasNamespace] of [
    ['setAttribute', false], ['setAttributeNS', true],
  ]) {
    const original = window.Element.prototype[method];
    window.Element.prototype[method] = hasNamespace
      ? function(namespace, name, value) { return original.call(this, namespace, name, mapAttribute(this, namespace, name, value)); }
      : function(name, value) { return original.call(this, name, mapAttribute(this, null, name, value)); };
  }
}
