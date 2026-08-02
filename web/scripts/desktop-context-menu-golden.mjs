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

const [bootstrap, controller, capability, workbench, codeBlock, markdownLink, markdownImage, fileReference, fileSpan, generatedImage, messageItem, toolCall, apiRoutes, desktopReveal] = await Promise.all([
  readFile(new URL('../src/bootstrap-app.ts', import.meta.url), 'utf8'),
  readFile(new URL('../src/lib/desktop-context-menu.ts', import.meta.url), 'utf8'),
  readFile(new URL('../../apps/desktop/capabilities/default.json', import.meta.url), 'utf8'),
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
  readFile(new URL('../../apps/desktop/src/file_reveal.rs', import.meta.url), 'utf8'),
]);

assert.match(bootstrap, /installDesktopContextMenu\(\)/, '应用启动时必须注册唯一桌面上下文菜单控制器');
assert.match(controller, /if \(!isDesktopRuntime\(\) \|\| removeDocumentListener\)/, '浏览器模式不得拦截原生右键菜单');
assert.match(controller, /document\.addEventListener\('contextmenu', handleContextMenu, true\)/, '桌面菜单必须在捕获阶段统一接管');
assert.match(controller, /event\.preventDefault\(\)[\s\S]*?resolveDesktopContextMenuRequest/, '桌面端必须阻止 WebView 默认菜单后再按语义解析');
assert.match(controller, /import\('@tauri-apps\/api\/menu'\)/, '桌面端必须使用 Tauri 原生菜单，而非 HTML 浮层');
assert.match(controller, /PredefinedMenuItem\.new/, '编辑能力必须使用系统预定义菜单项');
assert.match(controller, /kind\.startsWith\('image'\)/, '图片必须使用独立的原生菜单分支');
assert.match(controller, /contextMenu\.copyImageAddress/, '网络图片必须支持复制图片地址');
assert.match(controller, /resolveAgentFileRevealTarget/, '文件夹定位菜单必须先通过 daemon 解析真实文件');
assert.match(controller, /invoke\('reveal_workspace_file'/, '文件夹定位必须调用受限桌面命令');
assert.match(controller, /invoke\('open_workspace_folder'/, '打开工作区必须调用受限桌面命令');
assert.match(controller, /revealAvailable \? 'reveal' : 'plain'/, '菜单缓存必须区分可定位与普通文件状态');
assert.match(apiRoutes, /safe_workspace_path\(&workspace_root, file_path\)/, 'daemon 必须按工作区边界规范化定位目标');
assert.match(apiRoutes, /!target_path\.is_file\(\)/, 'daemon 不得把目录或特殊路径暴露为文件定位目标');
assert.match(desktopReveal, /canonical_target\.starts_with\(&canonical_workspace_root\)/, '桌面命令必须再次校验工作区边界');
assert.match(desktopReveal, /canonical_target\.is_file\(\)/, '桌面命令必须再次确认目标是文件');
assert.match(desktopReveal, /canonical_workspace_root\.is_dir\(\)/, '打开工作区前必须再次确认目标是目录');
assert.match(workbench, /use:desktopContextMenu=\{\{[\s\S]*?kind: 'workspace'[\s\S]*?workspacePathRef: workspaceBindingPath\(workspace\)/, '工作区标题必须声明打开文件夹菜单语义');

const desktopCapability = JSON.parse(capability);
assert.ok(desktopCapability.permissions.includes('core:default'), '桌面 capability 必须包含原生菜单权限');
assert.ok(desktopCapability.permissions.includes('allow-reveal-workspace-file'), '桌面 capability 必须仅授权受限文件定位命令');
assert.ok(desktopCapability.permissions.includes('allow-open-workspace-folder'), '桌面 capability 必须授权受限工作区目录打开命令');
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
