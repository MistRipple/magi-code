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
    }
    return url;
  };
  return {
    restore,
    resolve(input) {
      const value = String(input);
      if (!value || value.startsWith('#')) return value;
      const url = new URL(value, base);
      if (url.origin !== base.origin || !['http:', 'https:'].includes(url.protocol)
        || staticUrls.has(url.href) || url.pathname.startsWith(gateway.pathname)) return value;
      let path = url.pathname;
      if (path.startsWith(directory)) path = '/' + path.slice(directory.length);
      return gateway.origin + gateway.pathname + path.slice(1) + url.search + url.hash;
    },
  };
}

// 保留原生 <script>／<link>／<img> 的加载、CORS、模块相对导入与错误事件。
// 只转换其网络地址，不读取或改写页面组件、脚本正文、样式或图片。
export function installResourceUrls(window, urls) {
  const properties = new Map([
    ['SCRIPT', ['src']], ['LINK', ['href']], ['IMG', ['src']],
    ['SOURCE', ['src']], ['VIDEO', ['src', 'poster']], ['AUDIO', ['src']],
  ]);
  for (const [tag, names] of properties) {
    const prototype = window.document.createElement(tag).constructor.prototype;
    for (const name of names) {
      let owner = prototype, descriptor;
      while (owner && !(descriptor = Object.getOwnPropertyDescriptor(owner, name))) owner = Object.getPrototypeOf(owner);
      if (!descriptor?.get || !descriptor.set) continue;
      Object.defineProperty(prototype, name, {
        ...descriptor,
        set(value) { descriptor.set.call(this, urls.resolve(value)); },
      });
    }
  }
  const setAttribute = window.Element.prototype.setAttribute;
  window.Element.prototype.setAttribute = function(name, value) {
    return setAttribute.call(this, name, properties.get(this.tagName)?.includes(String(name).toLowerCase()) ? urls.resolve(value) : value);
  };
}
