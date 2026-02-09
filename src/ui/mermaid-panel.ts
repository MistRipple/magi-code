/**
 * MermaidPanel - Mermaid 图表独立面板
 * 在 VSCode Tab 页签中展示 Mermaid 图表
 *
 * 特性：
 * - 完整的拖拽和缩放交互（与对话区域一致）
 * - 持久化支持：重启 VS Code 后自动恢复
 * - 样式与 Svelte 版本统一
 */

import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';

// 持久化存储的面板数据
interface MermaidPanelState {
  code: string;
  title: string;
}

export class MermaidPanel {
  public static currentPanel: MermaidPanel | undefined;
  private static readonly viewType = 'multiCli.mermaidDiagram';

  private readonly _panel: vscode.WebviewPanel;
  private readonly _extensionUri: vscode.Uri;
  private _disposables: vscode.Disposable[] = [];
  private _code: string;
  private _title: string;

  /**
   * 创建或显示 Mermaid 面板
   */
  public static createOrShow(
    extensionUri: vscode.Uri,
    code: string,
    title?: string
  ): void {
    const column = vscode.window.activeTextEditor
      ? vscode.window.activeTextEditor.viewColumn
      : undefined;

    // 如果已有面板，更新内容并显示
    if (MermaidPanel.currentPanel) {
      MermaidPanel.currentPanel._code = code;
      MermaidPanel.currentPanel._title = title || 'Mermaid 图表';
      MermaidPanel.currentPanel._panel.reveal(column);
      MermaidPanel.currentPanel._update();
      MermaidPanel.currentPanel._saveState();
      return;
    }

    // 创建新面板
    const panel = vscode.window.createWebviewPanel(
      MermaidPanel.viewType,
      title || 'Mermaid 图表',
      column || vscode.ViewColumn.One,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(extensionUri, 'dist'),
          vscode.Uri.joinPath(extensionUri, 'node_modules'),
        ],
      }
    );

    MermaidPanel.currentPanel = new MermaidPanel(panel, extensionUri, code, title);
    MermaidPanel.currentPanel._saveState();
  }

  /**
   * 注册 Webview 序列化器（用于持久化恢复）
   */
  public static registerSerializer(context: vscode.ExtensionContext): void {
    vscode.window.registerWebviewPanelSerializer(MermaidPanel.viewType, {
      async deserializeWebviewPanel(webviewPanel: vscode.WebviewPanel, state: MermaidPanelState) {
        // 恢复面板
        if (state && state.code) {
          webviewPanel.title = state.title || 'Mermaid 图表';
          MermaidPanel.currentPanel = new MermaidPanel(
            webviewPanel,
            context.extensionUri,
            state.code,
            state.title
          );
        }
      }
    });
  }

  private constructor(
    panel: vscode.WebviewPanel,
    extensionUri: vscode.Uri,
    code: string,
    title?: string
  ) {
    this._panel = panel;
    this._extensionUri = extensionUri;
    this._code = code;
    this._title = title || 'Mermaid 图表';

    // 初始化 webview 内容
    this._update();

    // 监听面板关闭事件
    this._panel.onDidDispose(() => this.dispose(), null, this._disposables);

    // 监听面板可见性变化
    this._panel.onDidChangeViewState(
      () => {
        if (this._panel.visible) {
          this._update();
        }
      },
      null,
      this._disposables
    );

    // 处理来自 webview 的消息
    this._panel.webview.onDidReceiveMessage(
      (message) => {
        switch (message.type) {
          case 'ready':
            // Webview 已准备好，发送代码
            this._sendCode();
            break;
          case 'export':
            this._exportDiagram(message.format, message.data);
            break;
          case 'error':
            vscode.window.showErrorMessage(`Mermaid 渲染失败: ${message.error}`);
            break;
        }
      },
      null,
      this._disposables
    );
  }

  /**
   * 保存状态（用于持久化）
   */
  private _saveState(): void {
    // 通过 webview state API 保存
    this._panel.webview.postMessage({
      type: 'saveState',
      state: {
        code: this._code,
        title: this._title
      }
    });
  }

  /**
   * 发送 Mermaid 代码到 webview
   */
  private _sendCode(): void {
    this._panel.webview.postMessage({
      type: 'setCode',
      code: this._code,
      title: this._title,
    });
  }

  /**
   * 导出图表
   */
  private async _exportDiagram(format: 'svg' | 'png', data: string): Promise<void> {
    const defaultUri = vscode.Uri.file(
      path.join(
        vscode.workspace.workspaceFolders?.[0]?.uri.fsPath || '',
        `diagram.${format}`
      )
    );

    const uri = await vscode.window.showSaveDialog({
      defaultUri,
      filters: {
        [format.toUpperCase()]: [format],
      },
    });

    if (uri) {
      try {
        if (format === 'svg') {
          fs.writeFileSync(uri.fsPath, data);
        } else {
          // PNG 需要从 base64 解码
          const base64Data = data.replace(/^data:image\/png;base64,/, '');
          fs.writeFileSync(uri.fsPath, Buffer.from(base64Data, 'base64'));
        }
        vscode.window.showInformationMessage(`图表已保存: ${uri.fsPath}`);
      } catch (error) {
        vscode.window.showErrorMessage(`保存失败: ${error}`);
      }
    }
  }

  /**
   * 更新 webview 内容
   */
  private _update(): void {
    this._panel.title = this._title;
    this._panel.webview.html = this._getHtmlForWebview();
  }

  /**
   * 获取 webview HTML 内容
   * 包含完整的拖拽、缩放交互（与 MermaidRenderer.svelte 一致）
   */
  private _getHtmlForWebview(): string {
    const webview = this._panel.webview;
    const nonce = getNonce();

    return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}' https://cdn.jsdelivr.net; img-src ${webview.cspSource} data:; connect-src https://cdn.jsdelivr.net;">
  <title>${escapeHtml(this._title)}</title>
  <style>
    :root {
      --bg: #1e1e1e;
      --fg: #e0e0e0;
      --fg-muted: #888888;
      --border: #3c3c3c;
      --primary: #4a9eff;
      --info: #4a9eff;
      --surface-1: rgba(255,255,255,0.02);
      --surface-2: rgba(0,0,0,0.3);
      --surface-hover: rgba(255,255,255,0.1);
      --code-bg: rgba(0,0,0,0.3);
      --radius-sm: 4px;
      --radius-md: 8px;
      --space-1: 4px;
      --space-2: 8px;
      --space-3: 12px;
      --space-4: 16px;
      --text-sm: 13px;
      --text-xs: 11px;
    }

    * {
      box-sizing: border-box;
      margin: 0;
      padding: 0;
    }

    body {
      background: var(--bg);
      color: var(--fg);
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
      overflow: hidden;
      height: 100vh;
      display: flex;
      flex-direction: column;
    }

    /* 头部样式 - 与 MermaidRenderer.svelte 一致 */
    .mermaid-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: var(--space-2) var(--space-3);
      background: var(--surface-2);
      border-bottom: 1px solid var(--border);
    }

    .header-left {
      display: flex;
      align-items: center;
      gap: var(--space-2);
      color: var(--info);
      overflow: hidden;
    }

    .header-icon {
      width: 14px;
      height: 14px;
      flex-shrink: 0;
    }

    .header-type {
      font-size: var(--text-sm);
      font-weight: 500;
      flex-shrink: 0;
    }

    .header-title {
      font-size: var(--text-sm);
      color: var(--fg);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .header-actions {
      display: flex;
      align-items: center;
      gap: 4px;
      flex-shrink: 0;
    }

    .header-btn {
      display: flex;
      align-items: center;
      justify-content: center;
      width: 28px;
      height: 28px;
      background: transparent;
      border: none;
      color: var(--fg-muted);
      cursor: pointer;
      border-radius: var(--radius-sm);
      transition: all 0.15s;
    }

    .header-btn:hover {
      background: var(--surface-hover);
      color: var(--fg);
    }

    /* 图表区域 - 与 MermaidRenderer.svelte 一致 */
    .mermaid-content {
      position: relative;
      flex: 1;
      overflow: hidden;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: var(--space-4);
      background: var(--code-bg);
      cursor: grab;
    }

    .mermaid-content.dragging {
      cursor: grabbing;
    }

    .svg-wrapper {
      transform-origin: center center;
      transition: transform 0.05s ease-out;
      user-select: none;
    }

    .mermaid-content.dragging .svg-wrapper {
      transition: none;
    }

    .svg-wrapper svg {
      max-width: none;
      height: auto;
      display: block;
    }

    /* Mermaid SVG 样式 */
    .svg-wrapper .node rect,
    .svg-wrapper .node circle,
    .svg-wrapper .node ellipse,
    .svg-wrapper .node polygon,
    .svg-wrapper .node path {
      fill: #2d2d2d;
      stroke: #4a9eff;
    }

    .svg-wrapper .node .label,
    .svg-wrapper .nodeLabel,
    .svg-wrapper .label text,
    .svg-wrapper text {
      fill: #e0e0e0 !important;
      color: #e0e0e0 !important;
    }

    .svg-wrapper .edgePath path,
    .svg-wrapper .flowchart-link {
      stroke: #888888;
    }

    .svg-wrapper .edgeLabel,
    .svg-wrapper .edgeLabel text {
      fill: #e0e0e0;
      background-color: #2d2d2d;
    }

    .svg-wrapper .cluster rect {
      fill: #1a1a1a;
      stroke: #4a9eff;
    }

    .svg-wrapper marker path {
      fill: #888888;
    }

    /* 浮动控制按钮 - 与 MermaidRenderer.svelte 完全一致 */
    .floating-controls {
      position: absolute;
      bottom: var(--space-3);
      left: var(--space-3);
      display: flex;
      flex-direction: column;
      gap: var(--space-1);
    }

    .control-btn {
      display: flex;
      align-items: center;
      justify-content: center;
      width: 28px;
      height: 28px;
      background: var(--surface-2);
      backdrop-filter: blur(8px);
      border: 1px solid var(--border);
      color: var(--fg-muted);
      cursor: pointer;
      border-radius: var(--radius-sm);
      transition: all 0.15s;
      font-size: 16px;
    }

    .control-btn:hover {
      background: var(--surface-hover);
      color: var(--fg);
      border-color: var(--primary);
    }

    /* 右下角导出按钮 */
    .export-controls {
      position: absolute;
      bottom: var(--space-3);
      right: var(--space-3);
      display: flex;
      gap: var(--space-1);
    }

    .export-btn {
      display: flex;
      align-items: center;
      gap: 4px;
      padding: 6px 12px;
      background: var(--surface-2);
      backdrop-filter: blur(8px);
      border: 1px solid var(--border);
      color: var(--fg-muted);
      cursor: pointer;
      border-radius: var(--radius-sm);
      transition: all 0.15s;
      font-size: var(--text-xs);
    }

    .export-btn:hover {
      background: var(--surface-hover);
      color: var(--fg);
      border-color: var(--primary);
    }

    /* 加载状态 */
    .loading {
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: var(--space-2);
      color: var(--fg-muted);
      font-size: var(--text-sm);
    }

    .spinner {
      width: 24px;
      height: 24px;
      border: 2px solid var(--border);
      border-top-color: var(--primary);
      border-radius: 50%;
      animation: spin 1s linear infinite;
    }

    @keyframes spin { to { transform: rotate(360deg); } }

    /* 错误状态 */
    .error {
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: var(--space-2);
      padding: var(--space-4);
      color: #f44;
      text-align: center;
    }

    .error-title {
      font-weight: 500;
      font-size: var(--text-sm);
    }

    .error-message {
      font-family: 'Fira Code', 'SF Mono', Monaco, monospace;
      font-size: var(--text-xs);
      background: rgba(255, 68, 68, 0.1);
      padding: var(--space-2);
      border-radius: var(--radius-sm);
      max-width: 100%;
      overflow-x: auto;
      margin: 0;
    }
  </style>
</head>
<body>
  <!-- 头部 -->
  <div class="mermaid-header">
    <div class="header-left">
      <svg class="header-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <line x1="6" y1="3" x2="6" y2="15"></line>
        <circle cx="18" cy="6" r="3"></circle>
        <circle cx="6" cy="18" r="3"></circle>
        <path d="M18 9a9 9 0 0 1-9 9"></path>
      </svg>
      <span class="header-type" id="diagram-type">Mermaid</span>
      <span class="header-title" id="title"></span>
    </div>
    <div class="header-actions">
      <button class="header-btn" id="copy-svg" title="复制 SVG">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
        </svg>
      </button>
    </div>
  </div>

  <!-- 图表区域 -->
  <div class="mermaid-content" id="container">
    <div class="loading" id="loading">
      <span class="spinner"></span>
      <span>渲染中...</span>
    </div>
  </div>

  <!-- SVG 容器 -->
  <div class="svg-wrapper" id="wrapper" style="display: none;"></div>

  <!-- 浮动控制按钮 -->
  <div class="floating-controls" id="controls" style="display: none;">
    <button class="control-btn" id="zoom-in" title="放大">+</button>
    <button class="control-btn" id="zoom-out" title="缩小">−</button>
    <button class="control-btn" id="zoom-reset" title="重置视图">↻</button>
    <button class="control-btn" id="show-code" title="复制代码">{ }</button>
  </div>

  <!-- 导出按钮 -->
  <div class="export-controls" id="export-controls" style="display: none;">
    <button class="export-btn" id="export-svg">导出 SVG</button>
  </div>

  <script src="https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js" nonce="${nonce}"></script>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();

    // 状态
    let scale = 1;
    let translateX = 0;
    let translateY = 0;
    let isDragging = false;
    let dragStartX = 0;
    let dragStartY = 0;
    let initialTranslateX = 0;
    let initialTranslateY = 0;
    let svgContent = '';
    let currentCode = '';

    // 初始化 Mermaid（与 MermaidRenderer.svelte 完全一致）
    mermaid.initialize({
      startOnLoad: false,
      theme: 'dark',
      securityLevel: 'loose',
      fontFamily: 'ui-sans-serif, system-ui, sans-serif',
      themeVariables: {
        darkMode: true,
        background: '#1e1e1e',
        primaryColor: '#4a9eff',
        primaryTextColor: '#ffffff',
        primaryBorderColor: '#4a9eff',
        lineColor: '#888888',
        secondaryColor: '#2d5a8a',
        tertiaryColor: '#1a3a5c',
        textColor: '#e0e0e0',
        mainBkg: '#2d2d2d',
        nodeBorder: '#4a9eff',
        clusterBkg: '#1a1a1a',
        clusterBorder: '#4a9eff',
        titleColor: '#ffffff',
        edgeLabelBackground: '#2d2d2d',
      },
      flowchart: {
        htmlLabels: true,
        curve: 'basis',
        nodeSpacing: 50,
        rankSpacing: 50,
      },
      sequence: {
        diagramMarginX: 20,
        diagramMarginY: 20,
        actorMargin: 50,
        width: 150,
        height: 65,
      },
    });

    // 元素引用
    const container = document.getElementById('container');
    const wrapper = document.getElementById('wrapper');
    const loading = document.getElementById('loading');
    const controls = document.getElementById('controls');
    const exportControls = document.getElementById('export-controls');
    const titleEl = document.getElementById('title');
    const typeEl = document.getElementById('diagram-type');

    // 更新变换
    function updateTransform() {
      wrapper.style.transform = 'translate(' + translateX + 'px, ' + translateY + 'px) scale(' + scale + ')';
    }

    // 缩放控制
    document.getElementById('zoom-in').onclick = function() {
      scale = Math.min(scale * 1.2, 15);
      updateTransform();
    };

    document.getElementById('zoom-out').onclick = function() {
      scale = Math.max(scale / 1.2, 0.3);
      updateTransform();
    };

    document.getElementById('zoom-reset').onclick = function() {
      scale = 1;
      translateX = 0;
      translateY = 0;
      updateTransform();
    };

    // 复制代码
    document.getElementById('show-code').onclick = function() {
      if (currentCode) {
        navigator.clipboard.writeText(currentCode).then(function() {
          const btn = document.getElementById('show-code');
          btn.innerHTML = '✓';
          setTimeout(function() { btn.innerHTML = '{ }'; }, 1500);
        });
      }
    };

    // 拖拽控制
    container.addEventListener('mousedown', function(e) {
      isDragging = true;
      dragStartX = e.clientX;
      dragStartY = e.clientY;
      initialTranslateX = translateX;
      initialTranslateY = translateY;
      container.classList.add('dragging');
      e.preventDefault();
    });

    document.addEventListener('mousemove', function(e) {
      if (isDragging) {
        translateX = initialTranslateX + (e.clientX - dragStartX);
        translateY = initialTranslateY + (e.clientY - dragStartY);
        updateTransform();
      }
    });

    document.addEventListener('mouseup', function() {
      isDragging = false;
      container.classList.remove('dragging');
    });

    // 滚轮缩放
    container.addEventListener('wheel', function(e) {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      scale = Math.min(Math.max(scale * delta, 0.3), 15);
      updateTransform();
    }, { passive: false });

    // 复制 SVG
    document.getElementById('copy-svg').onclick = async function() {
      if (svgContent) {
        try {
          await navigator.clipboard.writeText(svgContent);
          this.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#4a9eff" stroke-width="2"><polyline points="20 6 9 17 4 12"></polyline></svg>';
          setTimeout(() => {
            this.innerHTML = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path></svg>';
          }, 2000);
        } catch (e) {
          console.error('复制失败:', e);
        }
      }
    };

    // 导出 SVG
    document.getElementById('export-svg').onclick = function() {
      if (svgContent) {
        vscode.postMessage({ type: 'export', format: 'svg', data: svgContent });
      }
    };

    // 检测图表类型
    function detectDiagramType(code) {
      const typePatterns = [
        [/^\\s*flowchart/mi, '流程图'],
        [/^\\s*graph/mi, '流程图'],
        [/^\\s*sequenceDiagram/mi, '时序图'],
        [/^\\s*classDiagram/mi, '类图'],
        [/^\\s*stateDiagram/mi, '状态图'],
        [/^\\s*erDiagram/mi, 'ER 图'],
        [/^\\s*gantt/mi, '甘特图'],
        [/^\\s*pie/mi, '饼图'],
        [/^\\s*journey/mi, '用户旅程'],
        [/^\\s*gitGraph/mi, 'Git 图'],
        [/^\\s*mindmap/mi, '思维导图'],
        [/^\\s*timeline/mi, '时间线'],
      ];

      for (const [pattern, type] of typePatterns) {
        if (pattern.test(code)) return type;
      }
      return 'Mermaid';
    }

    // 从代码中提取标题
    function extractTitle(code) {
      if (!code) return '';

      // YAML frontmatter 格式
      const yamlMatch = code.match(/^---\\s*\\n(?:.*\\n)*?title:\\s*(.+?)\\n(?:.*\\n)*?---/m);
      if (yamlMatch) return yamlMatch[1].trim();

      // accTitle 格式
      const accMatch = code.match(/accTitle:\\s*(.+?)(?:\\n|$)/);
      if (accMatch) return accMatch[1].trim();

      // 注释格式
      const commentMatch = code.match(/(?:flowchart|graph|sequenceDiagram|classDiagram|stateDiagram|erDiagram|gantt|pie|mindmap|timeline).*?\\n\\s*%%\\s*(.+?)(?:\\n|$)/);
      if (commentMatch) return commentMatch[1].trim();

      // 思维导图根节点
      const mindmapMatch = code.match(/^\\s*mindmap\\s*\\n\\s*root\\s*(?:\\(\\((.+?)\\)\\)|\\((.+?)\\)|\\[(.+?)\\]|(.+?)(?:\\n|$))/m);
      if (mindmapMatch) {
        const rootText = mindmapMatch[1] || mindmapMatch[2] || mindmapMatch[3] || mindmapMatch[4];
        if (rootText) return rootText.trim();
      }

      // 流程图第一个节点
      const flowchartMatch = code.match(/(?:flowchart|graph)\\s+(?:TD|TB|BT|RL|LR)\\s*\\n\\s*\\w+\\s*(?:\\[\\[(.+?)\\]\\]|\\[(.+?)\\]|\\(\\((.+?)\\)\\)|\\((.+?)\\)|\\{(.+?)\\})/m);
      if (flowchartMatch) {
        const nodeText = flowchartMatch[1] || flowchartMatch[2] || flowchartMatch[3] || flowchartMatch[4] || flowchartMatch[5];
        if (nodeText) return nodeText.trim();
      }

      return '';
    }

    // 渲染图表
    async function renderDiagram(code, title) {
      currentCode = code;

      // 如果没有传入标题，尝试从代码中提取
      const displayTitle = title || extractTitle(code);
      if (displayTitle) {
        titleEl.textContent = displayTitle;
      }

      typeEl.textContent = detectDiagramType(code);

      try {
        const id = 'mermaid-' + Date.now();
        const { svg } = await mermaid.render(id, code.trim());
        svgContent = svg;

        loading.style.display = 'none';
        wrapper.innerHTML = svg;
        wrapper.style.display = 'block';
        container.appendChild(wrapper);
        controls.style.display = 'flex';
        exportControls.style.display = 'flex';

        updateTransform();

        // 保存状态用于持久化
        vscode.setState({ code: code, title: title || '' });
      } catch (e) {
        loading.innerHTML = '<div class="error"><span class="error-title">渲染失败</span><pre class="error-message">' + escapeHtml(e.message || '未知错误') + '</pre></div>';
        vscode.postMessage({ type: 'error', error: e.message || '未知错误' });
      }
    }

    function escapeHtml(str) {
      return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    }

    // 监听来自扩展的消息
    window.addEventListener('message', function(event) {
      const message = event.data;
      if (message.type === 'setCode') {
        renderDiagram(message.code, message.title);
      } else if (message.type === 'saveState') {
        vscode.setState(message.state);
      }
    });

    // 尝试从保存的状态恢复
    const previousState = vscode.getState();
    if (previousState && previousState.code) {
      renderDiagram(previousState.code, previousState.title);
    } else {
      // 通知扩展已准备就绪
      vscode.postMessage({ type: 'ready' });
    }
  </script>
</body>
</html>`;
  }

  /**
   * 释放资源
   */
  public dispose(): void {
    MermaidPanel.currentPanel = undefined;

    this._panel.dispose();

    while (this._disposables.length) {
      const x = this._disposables.pop();
      if (x) {
        x.dispose();
      }
    }
  }
}

/**
 * 生成随机 nonce
 */
function getNonce(): string {
  let text = '';
  const possible = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}

/**
 * HTML 转义
 */
function escapeHtml(str: string): string {
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}
