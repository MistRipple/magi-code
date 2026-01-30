/**
 * MermaidPanel - Mermaid 图表独立面板
 * 在 VSCode Tab 页签中展示 Mermaid 图表
 */

import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';

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
      --border: #3c3c3c;
      --primary: #4a9eff;
      --surface: #252526;
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

    .toolbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 8px 16px;
      background: var(--surface);
      border-bottom: 1px solid var(--border);
    }

    .toolbar-left {
      display: flex;
      align-items: center;
      gap: 12px;
    }

    .toolbar-title {
      font-size: 14px;
      font-weight: 500;
      color: var(--primary);
    }

    .toolbar-actions {
      display: flex;
      gap: 8px;
    }

    .btn {
      display: flex;
      align-items: center;
      gap: 4px;
      padding: 6px 12px;
      background: transparent;
      border: 1px solid var(--border);
      color: var(--fg);
      border-radius: 4px;
      cursor: pointer;
      font-size: 12px;
      transition: all 0.15s;
    }

    .btn:hover {
      background: rgba(74, 158, 255, 0.1);
      border-color: var(--primary);
    }

    .btn-icon {
      padding: 6px;
      min-width: 32px;
      justify-content: center;
    }

    .zoom-level {
      font-size: 12px;
      color: #888;
      min-width: 50px;
      text-align: center;
    }

    .diagram-container {
      flex: 1;
      overflow: auto;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--bg);
    }

    .diagram-wrapper {
      transform-origin: center center;
      transition: transform 0.15s ease-out;
    }

    .diagram-wrapper svg {
      max-width: 100%;
      height: auto;
      display: block;
    }

    .loading {
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: 12px;
      color: #888;
    }

    .spinner {
      width: 32px;
      height: 32px;
      border: 3px solid var(--border);
      border-top-color: var(--primary);
      border-radius: 50%;
      animation: spin 1s linear infinite;
    }

    @keyframes spin {
      to { transform: rotate(360deg); }
    }

    .error {
      color: #f44;
      text-align: center;
      padding: 24px;
    }

    .error pre {
      margin-top: 12px;
      padding: 12px;
      background: rgba(255, 68, 68, 0.1);
      border-radius: 4px;
      font-size: 12px;
      overflow-x: auto;
    }
  </style>
</head>
<body>
  <div class="toolbar">
    <div class="toolbar-left">
      <span class="toolbar-title" id="title">Mermaid 图表</span>
    </div>
    <div class="toolbar-actions">
      <button class="btn btn-icon" id="zoom-out" title="缩小">−</button>
      <span class="zoom-level" id="zoom-level">100%</span>
      <button class="btn btn-icon" id="zoom-in" title="放大">+</button>
      <button class="btn btn-icon" id="zoom-reset" title="重置">⟲</button>
      <button class="btn" id="copy-svg">复制 SVG</button>
      <button class="btn" id="export-svg">导出 SVG</button>
    </div>
  </div>

  <div class="diagram-container" id="container">
    <div class="loading">
      <div class="spinner"></div>
      <span>加载中...</span>
    </div>
  </div>

  <div class="diagram-wrapper" id="wrapper" style="display: none;"></div>

  <script src="https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js" nonce="${nonce}"></script>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();

    // 状态
    let scale = 1;
    let svgContent = '';

    // 初始化 Mermaid
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
    });

    // 元素引用
    const container = document.getElementById('container');
    const wrapper = document.getElementById('wrapper');
    const zoomLevel = document.getElementById('zoom-level');
    const titleEl = document.getElementById('title');

    // 缩放控制
    function updateZoom() {
      zoomLevel.textContent = Math.round(scale * 100) + '%';
      wrapper.style.transform = 'scale(' + scale + ')';
    }

    document.getElementById('zoom-in').onclick = function() {
      scale = Math.min(scale * 1.2, 3);
      updateZoom();
    };

    document.getElementById('zoom-out').onclick = function() {
      scale = Math.max(scale / 1.2, 0.3);
      updateZoom();
    };

    document.getElementById('zoom-reset').onclick = function() {
      scale = 1;
      updateZoom();
    };

    // 复制 SVG
    document.getElementById('copy-svg').onclick = async function() {
      if (svgContent) {
        try {
          await navigator.clipboard.writeText(svgContent);
          this.textContent = '已复制!';
          setTimeout(() => { this.textContent = '复制 SVG'; }, 2000);
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

    // 渲染图表
    async function renderDiagram(code, title) {
      if (title) {
        titleEl.textContent = title;
      }

      try {
        const id = 'mermaid-' + Date.now();
        const { svg } = await mermaid.render(id, code.trim());
        svgContent = svg;

        container.innerHTML = '';
        wrapper.innerHTML = svg;
        wrapper.style.display = 'block';
        container.appendChild(wrapper);

        updateZoom();
      } catch (e) {
        container.innerHTML = '<div class="error"><p>渲染失败</p><pre>' + escapeHtml(e.message || '未知错误') + '</pre></div>';
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
      }
    });

    // 通知扩展已准备就绪
    vscode.postMessage({ type: 'ready' });
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
