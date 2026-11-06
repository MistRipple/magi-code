// 发行期构建的浏览器模块目录；共享 React，安装时不编译、不 eval。
export const SHARED_BROWSER_MODULES = ['react', 'react-dom', 'react/jsx-runtime'];
export const BROWSER_LIBRARIES = [
  { name: 'shiki/core', file: 'syntax-core.js', dependencies: [] },
  { name: 'shiki/engine/javascript', file: 'syntax-engine.js', dependencies: [] },
  { name: '@shikijs/langs/typescript', file: 'syntax-typescript.js', dependencies: [] },
  { name: '@shikijs/langs/shellscript', file: 'syntax-shell.js', dependencies: [] },
  { name: '@shikijs/langs/json', file: 'syntax-json.js', dependencies: [] },
  { name: '@deepseek-ai/dsh-client-ui-primitives', file: 'ui-primitives.js', dependencies: ['shiki/core', 'shiki/engine/javascript', '@shikijs/langs/typescript', '@shikijs/langs/shellscript', '@shikijs/langs/json'] },
];
export function browserLibraries(names) {
  const selected = new Set();
  const add = (name) => {
    if (SHARED_BROWSER_MODULES.includes(name)) return;
    const library = BROWSER_LIBRARIES.find((item) => item.name === name);
    if (!library) throw new Error(`Magi 尚未提供浏览器模块 ${name}`);
    if (selected.has(name)) return;
    for (const dependency of library.dependencies) add(dependency);
    selected.add(name);
  };
  for (const name of names) add(name);
  return BROWSER_LIBRARIES.filter((item) => selected.has(item.name));
}
