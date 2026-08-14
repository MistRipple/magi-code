import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

class MockNode {}
class MockHTMLElement extends MockNode {
  constructor() {
    super();
    this.isContentEditable = false;
  }
}
class MockInputElement extends MockHTMLElement {
  constructor({ type = 'text', disabled = false, readOnly = false, selectionStart = 0, selectionEnd = 0 } = {}) {
    super();
    this.type = type;
    this.disabled = disabled;
    this.readOnly = readOnly;
    this.selectionStart = selectionStart;
    this.selectionEnd = selectionEnd;
  }
}
class MockTextAreaElement extends MockInputElement {}

globalThis.Node = MockNode;
globalThis.HTMLElement = MockHTMLElement;
globalThis.HTMLInputElement = MockInputElement;
globalThis.HTMLTextAreaElement = MockTextAreaElement;

let selectedTarget = null;
globalThis.window = {
  getSelection: () => selectedTarget
    ? {
        isCollapsed: false,
        toString: () => 'selected text',
        containsNode: (target) => target === selectedTarget,
      }
    : {
        isCollapsed: true,
        toString: () => '',
        containsNode: () => false,
      },
};

function contextEvent(target, path = [target]) {
  return {
    target,
    composedPath: () => path,
  };
}

await withGoldenViteServer(async (server) => {
  const contextMenu = await server.ssrLoadModule('/src/lib/desktop-context-menu-contract.ts');
  const resolve = contextMenu.resolveDesktopContextMenuRequest;

  assert.equal(
    resolve(contextEvent(new MockInputElement()))?.kind,
    'editable',
    '可编辑输入必须使用完整系统编辑菜单',
  );
  assert.equal(
    resolve(contextEvent(new MockInputElement({ readOnly: true })))?.kind,
    'readonly',
    '只读输入必须保留复制和全选菜单',
  );
  assert.equal(
    resolve(contextEvent(new MockInputElement({ type: 'checkbox' }))),
    null,
    '非文本输入不得显示编辑菜单',
  );

  const selectedText = new MockHTMLElement();
  selectedTarget = selectedText;
  assert.equal(resolve(contextEvent(selectedText))?.kind, 'selection', '当前目标内的文本选区必须支持复制');
  assert.equal(
    resolve(contextEvent(new MockHTMLElement())),
    null,
    '其他区域残留的文本选区不得污染当前右键菜单',
  );

  selectedTarget = null;
  const workspace = new MockHTMLElement();
  contextMenu.desktopContextMenu(workspace, {
    kind: 'workspace',
    workspacePathRef: 'mhp1:u:L3dvcmtzcGFjZQ',
  });
  assert.equal(resolve(contextEvent(workspace))?.kind, 'workspace', '工作区必须提供独立的文件夹操作菜单');

  const link = new MockHTMLElement();
  contextMenu.desktopContextMenu(link, { kind: 'link', url: 'https://example.com', open: () => undefined });
  assert.equal(resolve(contextEvent(link))?.kind, 'link', '网页链接必须提供打开和复制链接菜单');

  const file = new MockHTMLElement();
  const fileScope = { workspaceId: 'workspace-main', workspacePath: '/workspace' };
  contextMenu.desktopContextMenu(file, {
    kind: 'file',
    filePath: 'src/main.rs',
    open: () => undefined,
    fileScope,
  });
  const fileRequest = resolve(contextEvent(file));
  assert.equal(fileRequest?.kind, 'file', '可打开文件必须提供打开和复制路径菜单');
  assert.deepEqual(fileRequest?.descriptor?.fileScope, fileScope, '文件菜单必须保留后端校验所需的工作区作用域');

  const readonlyFile = new MockHTMLElement();
  contextMenu.desktopContextMenu(readonlyFile, { kind: 'file', filePath: 'src/main.rs' });
  assert.equal(resolve(contextEvent(readonlyFile))?.kind, 'file-copy', '不可打开文件只能提供复制路径菜单');

  const code = new MockHTMLElement();
  const codeChild = new MockHTMLElement();
  contextMenu.desktopContextMenu(code, {
    kind: 'code',
    content: 'fn main() {}',
    filePath: 'src/main.rs',
    openFile: () => undefined,
  });
  assert.equal(
    resolve(contextEvent(codeChild, [codeChild, code]))?.kind,
    'code-file',
    '代码块必须通过组合路径继承复制代码和文件操作语义',
  );
  selectedTarget = codeChild;
  assert.equal(
    resolve(contextEvent(codeChild, [codeChild, code]))?.kind,
    'code-file-selection',
    '代码选区必须优先复制选中内容并保留文件操作',
  );

  selectedTarget = null;
  const localImage = new MockHTMLElement();
  contextMenu.desktopContextMenu(localImage, {
    kind: 'image',
    filePath: 'images/output.png',
    fileScope,
    open: () => undefined,
  });
  assert.equal(resolve(contextEvent(localImage))?.kind, 'image-file', '本地图片必须提供图片与文件操作菜单');

  const remoteImage = new MockHTMLElement();
  contextMenu.desktopContextMenu(remoteImage, {
    kind: 'image',
    source: 'https://example.com/output.png',
    open: () => undefined,
  });
  assert.equal(resolve(contextEvent(remoteImage))?.kind, 'image-source', '网络图片必须提供打开与复制地址菜单');

  const previewImage = new MockHTMLElement();
  contextMenu.desktopContextMenu(previewImage, { kind: 'image', open: () => undefined });
  assert.equal(resolve(contextEvent(previewImage))?.kind, 'image-open', '内嵌图片必须至少保留打开预览操作');
});

const [bootstrap, controller, workbench, codeBlock, markdownLink, markdownImage, fileReference, fileSpan, generatedImage, messageItem, toolCall, apiRoutes, desktopMain, desktopFiles, desktopPreload] = await Promise.all([
  readFile(new URL('../src/bootstrap-app.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/lib/desktop-context-menu.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/CodeBlock.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/renderers/MdLink.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/renderers/MdImage.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/renderers/FileReferenceInline.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/FileSpan.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/GeneratedImageBlock.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/MessageItem.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../src/components/ToolCall.svelte', import.meta.url), 'utf8'),
  readFile(new URL('../../crates/magi-api/src/routes/changes_files_tunnel.rs', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/src/main/index.ts', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/src/main/desktop-files.ts', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/src/preload/index.ts', import.meta.url), 'utf8'),
]);

assert.match(bootstrap, /installDesktopContextMenu\(\)/, '应用启动时必须注册唯一桌面上下文菜单控制器');
assert.match(controller, /if \(!window\.magiDesktop \|\| removeDocumentListener\)/, '浏览器模式不得拦截原生右键菜单');
assert.match(controller, /document\.addEventListener\('contextmenu', handleContextMenu, true\)/, '桌面菜单必须在捕获阶段统一接管');
assert.match(controller, /resolveDesktopContextMenuRequest\(event\)[\s\S]*?event\.preventDefault\(\)/, '桌面端只在解析出菜单语义后阻止默认菜单');
assert.match(controller, /desktop\.showContextMenu\(\{ items \}\)/, 'Renderer 必须通过受限 preload 契约请求原生菜单');
assert.match(controller, /role\('undo'\)[\s\S]*?role\('copy'\)[\s\S]*?role\('paste'\)/, '编辑能力必须声明 Electron 系统角色');
assert.match(controller, /descriptor\?\.kind === 'image'/, '图片必须使用独立的原生菜单分支');
assert.match(controller, /contextMenu\.copyImageAddress/, '网络图片必须支持复制图片地址');
assert.match(controller, /resolveAgentFileRevealTarget/, '文件夹定位菜单必须先通过 daemon 解析真实文件');
assert.match(controller, /desktop\.revealWorkspaceFile/, '文件夹定位必须调用受限 Electron 桥接');
assert.match(controller, /desktop\.openWorkspaceFolder/, '打开工作区必须调用受限 Electron 桥接');
assert.doesNotMatch(controller, /@tauri-apps|menuCache|HTML/, 'Renderer 不得保留 Tauri 菜单或第二套菜单实现');
assert.match(desktopMain, /Menu\.buildFromTemplate[\s\S]*?menu\.popup/, 'Electron Main 必须创建并弹出系统原生菜单');
assert.match(desktopMain, /allowedRoles[\s\S]*?undo[\s\S]*?selectAll/, 'Main 必须限制 Renderer 可请求的编辑角色');
assert.match(desktopPreload, /showContextMenu:[\s\S]*?magi-desktop:show-context-menu/, 'preload 必须只暴露受限菜单 IPC');
assert.match(apiRoutes, /safe_workspace_path\(&workspace_root, file_path\)/, 'daemon 必须按工作区边界规范化定位目标');
assert.match(apiRoutes, /!target_path\.is_file\(\)/, 'daemon 不得把目录或特殊路径暴露为文件定位目标');
assert.match(desktopFiles, /relative\(workspaceRoot, targetPath\)[\s\S]*?startsWith\(`\.\.\$\{sep\}`\)/, '桌面命令必须再次校验工作区边界');
assert.match(desktopFiles, /canonicalPath\(input\.targetPathRef, "file"\)/, '桌面命令必须再次确认目标是文件');
assert.match(desktopFiles, /canonicalPath\(workspaceRootPathRef, "directory"\)/, '打开工作区前必须再次确认目标是目录');
assert.match(workbench, /use:desktopContextMenu=\{\{[\s\S]*?kind: 'workspace'[\s\S]*?workspacePathRef: workspaceBindingPath\(workspace\)/, '工作区标题必须声明打开文件夹菜单语义');

for (const [source, label] of [
  [codeBlock, '代码块'],
  [markdownLink, 'Markdown 链接'],
  [fileReference, 'Markdown 文件引用'],
  [fileSpan, '通用文件引用'],
]) {
  assert.match(source, /use:desktopContextMenu=/, `${label}必须声明明确的上下文语义`);
}
for (const [source, label] of [
  [markdownImage, 'Markdown 图片'],
  [generatedImage, '生成图片'],
  [messageItem, '用户上传图片'],
  [toolCall, '工具输出图片'],
]) {
  assert.match(source, /use:desktopContextMenu=/, `${label}必须声明图片上下文语义`);
}

console.log('desktop context menu golden replay passed');
