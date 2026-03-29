/**
 * WebviewProvider - VS Code 插件壳层
 * 负责：
 * 1. 注入 Agent 单链运行所需的初始绑定信息
 * 2. 处理必须回到 VS Code 宿主执行的本地能力
 * 3. 通过 Agent API 创建会话并把 bootstrap 推回前端
 * 4. 通用 API 代理 + SSE 事件转发（WebView 无法直接访问 localhost）
 */

import { logger, LogCategory } from '../logging';
import * as crypto from 'crypto';
import * as http from 'http';
import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { t, setLocale as setExtensionLocale, type LocaleCode } from '../i18n';
import { ConfigManager } from '../config';
import {
  createDataMessage,
  createNotifyMessage,
  type DataMessageType,
  type NotifyLevel,
} from '../protocol/message-protocol';
import type {
  ExtensionToWebviewMessage,
  WebviewToExtensionMessage,
} from '../types';
import type { SessionBootstrapSnapshot } from '../shared/session-bootstrap';
import { MermaidPanel } from './mermaid-panel';
import { WorkspaceRoots, type WorkspaceFolderInfo } from '../workspace/workspace-roots';

function getNonce(): string {
  return crypto.randomBytes(16).toString('hex');
}

interface AgentBootstrapPayload extends SessionBootstrapSnapshot {
  agent?: {
    runtimeEpoch?: string;
    version?: string;
    baseUrl?: string;
    port?: number;
    platform?: string;
  };
  workspace: {
    workspaceId: string;
    name: string;
    rootPath: string;
  };
}

interface AgentFilePreviewPayload {
  filePath: string;
  absolutePath: string;
  exists: boolean;
  content: string;
  language: string;
}

interface AgentChangeDiffPayload {
  filePath: string;
  diff: string;
  additions: number;
  deletions: number;
  originalContent?: string;
  currentContent?: string;
  currentAbsolutePath?: string;
  currentExists?: boolean;
}

interface AgentConnectionController {
  ensureReadyBaseUrl(): Promise<string>;
}

export class WebviewProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = 'magi.mainView';

  private readonly workspaceFolders: WorkspaceFolderInfo[];
  private readonly workspaceRoots: WorkspaceRoots;
  private readonly workspaceRoot: string;
  private readonly workspaceId: string;
  private readonly locale: LocaleCode;

  private _view?: vscode.WebviewView;
  private postQueue: Promise<void> = Promise.resolve();
  private readonly tempFiles = new Set<string>();
  private activeSseRequest: http.ClientRequest | null = null;
  private activeSseSubscriptionId: string | null = null;
  private currentSessionId = '';

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly context: vscode.ExtensionContext,
    workspaceFolders: WorkspaceFolderInfo[],
    private agentBaseUrl: string,
    private readonly agentController: AgentConnectionController,
  ) {
    const normalizedFolders = workspaceFolders
      .filter((folder) => folder && folder.path)
      .map((folder) => ({ name: folder.name, path: folder.path }));

    if (normalizedFolders.length === 0) {
      throw new Error(t('provider.errors.noWorkspaceDetected'));
    }

    const normalizedAgentBaseUrl = agentBaseUrl.trim();
    if (!normalizedAgentBaseUrl) {
      throw new Error('[WebviewProvider] Agent-only 模式要求提供 agentBaseUrl');
    }

    this.workspaceFolders = normalizedFolders;
    this.workspaceRoots = new WorkspaceRoots(normalizedFolders);
    this.workspaceRoot = this.workspaceRoots.getPrimaryFolder().path;
    this.workspaceId = Buffer.from(path.resolve(this.workspaceRoot)).toString('base64url');
    this.agentBaseUrl = normalizedAgentBaseUrl;
    this.locale = this.normalizeLocaleCode(ConfigManager.getInstance().get('locale'));
    setExtensionLocale(this.locale);

    logger.info('界面.WebviewProvider.Agent壳已启用', {
      agentBaseUrl: this.agentBaseUrl,
      workspaceRoot: this.workspaceRoot,
      workspaceId: this.workspaceId,
    }, LogCategory.UI);
  }

  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken,
  ): void {
    this._view = webviewView;

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [this.extensionUri],
      // 不再需要 portMapping：所有 Agent API 请求通过宿主 postMessage 代理
    };

    try {
      webviewView.webview.html = this.getHtmlContent(webviewView.webview);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      logger.error('界面.Webview.加载失败', { error: message }, LogCategory.UI);
      webviewView.webview.html = this.getWebviewErrorHtml(message);
      void vscode.window.showErrorMessage(t('provider.errors.webviewRenderFailed', { error: message }));
      return;
    }

    webviewView.webview.onDidReceiveMessage(
      (message: WebviewToExtensionMessage) => {
        void this.handleMessage(message);
      },
      undefined,
      this.context.subscriptions,
    );

    logger.info('界面.Webview.插件壳已就绪', {
      workspaceRoot: this.workspaceRoot,
      workspaceId: this.workspaceId,
      agentBaseUrl: this.agentBaseUrl,
    }, LogCategory.UI);
  }

  public async createNewSession(): Promise<void> {
    const payload = await this.fetchAgentJson<AgentBootstrapPayload>('/api/session/new', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        workspaceId: this.workspaceId,
        workspacePath: this.workspaceRoot,
      }),
    });

    this.currentSessionId = payload.sessionId?.trim() || '';
    this.sendData('sessionBootstrapLoaded', payload as unknown as Record<string, unknown>, payload.sessionId);
    logger.info('界面.会话.Agent新建完成', {
      sessionId: payload.sessionId,
      workspaceRoot: this.workspaceRoot,
      workspaceId: this.workspaceId,
    }, LogCategory.UI);
  }

  public async refreshAgentBaseUrl(nextBaseUrl: string): Promise<void> {
    this.updateAgentBaseUrl(nextBaseUrl);
    if (this._view) {
      this._view.webview.html = this.getHtmlContent(this._view.webview);
    }
  }

  public async dispose(): Promise<void> {
    this.closeSseForwarding();
    this._view = undefined;
    for (const tempFile of this.tempFiles) {
      try {
        fs.rmSync(tempFile, { force: true });
      } catch {
        // ignore temp cleanup error
      }
    }
    this.tempFiles.clear();
  }

  private async handleMessage(message: WebviewToExtensionMessage): Promise<void> {
    switch (message.type) {
      case 'openFile':
        await this.handleOpenFile(message);
        return;
      case 'viewDiff':
        await this.openVscodeDiff(message);
        return;
      case 'openLink':
        await this.handleOpenLink(message.url);
        return;
      case 'openMermaidPanel':
        this.handleOpenMermaidPanel(message.code, message.title);
        return;
      case 'agentApiProxy':
        void this.handleAgentApiProxy(message);
        return;
      case 'agentSseSubscribe':
        void this.startSseForwarding(message.queryString, message.subscriptionId).catch((error) => {
          logger.error('界面.SSE代理.建立失败', {
            error: error instanceof Error ? error.message : String(error),
            subscriptionId: message.subscriptionId,
          }, LogCategory.UI);
          this.postMessage({
            type: 'agentSseStatus',
            status: 'error',
            subscriptionId: message.subscriptionId,
          } as ExtensionToWebviewMessage);
        });
        return;
      case 'agentSseUnsubscribe':
        this.closeSseForwarding(message.subscriptionId);
        return;
      default:
        logger.debug('界面.Webview.宿主忽略非壳层消息', { type: message.type }, LogCategory.UI);
    }
  }

  // ============================================
  // 通用 API 代理：WebView postMessage → 宿主 fetch → Agent
  // ============================================

  private async handleAgentApiProxy(
    message: Extract<WebviewToExtensionMessage, { type: 'agentApiProxy' }>,
  ): Promise<void> {
    const { requestId, method, url, body, headers } = message;
    try {
      // 将前端发来的完整 URL 中的 agentBaseUrl 部分替换为宿主侧的实际地址
      // 前端拼接的 URL 可能是 http://localhost:46231/api/...，宿主侧直接用 agentBaseUrl
      const agentBaseUrlObj = new URL(await this.resolveActiveAgentBaseUrl());
      const requestUrl = new URL(url);
      requestUrl.protocol = agentBaseUrlObj.protocol;
      requestUrl.host = agentBaseUrlObj.host;

      const fetchHeaders: Record<string, string> = { ...headers };
      const response = await fetch(requestUrl.toString(), {
        method,
        headers: fetchHeaders,
        body: body || undefined,
      });
      const responseBody = await response.text();
      const responseHeaders: Record<string, string> = {};
      response.headers.forEach((value, key) => {
        responseHeaders[key] = value;
      });
      this.postMessage({
        type: 'agentApiProxyResponse',
        requestId,
        status: response.status,
        body: responseBody,
        headers: responseHeaders,
      } as ExtensionToWebviewMessage);
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      logger.error('界面.API代理.请求失败', { requestId, url, error: errorMessage }, LogCategory.UI);
      this.postMessage({
        type: 'agentApiProxyResponse',
        requestId,
        status: 502,
        body: JSON.stringify({ error: errorMessage }),
        headers: { 'content-type': 'application/json' },
      } as ExtensionToWebviewMessage);
    }
  }

  // ============================================
  // SSE 事件转发：宿主 http.get → Agent /api/events → postMessage → WebView
  // ============================================

  private async startSseForwarding(queryString: string, subscriptionId: string): Promise<void> {
    this.closeSseForwarding();
    this.activeSseSubscriptionId = subscriptionId;
    const sseUrl = `${await this.resolveActiveAgentBaseUrl()}/api/events?${queryString}`;
    logger.info('界面.SSE代理.建立连接', { url: sseUrl, subscriptionId }, LogCategory.UI);

    const parsedUrl = new URL(sseUrl);
    const isActiveSubscription = () => this.activeSseSubscriptionId === subscriptionId;
    const request = http.get(
      {
        hostname: parsedUrl.hostname,
        port: parsedUrl.port,
        path: `${parsedUrl.pathname}${parsedUrl.search}`,
        headers: { Accept: 'text/event-stream', 'Cache-Control': 'no-cache' },
      },
      (response) => {
        if (!isActiveSubscription()) {
          response.resume();
          return;
        }
        if (response.statusCode !== 200) {
          logger.error('界面.SSE代理.非200响应', {
            status: response.statusCode,
            subscriptionId,
          }, LogCategory.UI);
          this.postMessage({
            type: 'agentSseStatus',
            status: 'error',
            subscriptionId,
          } as ExtensionToWebviewMessage);
          response.resume();
          return;
        }
        this.postMessage({
          type: 'agentSseStatus',
          status: 'open',
          subscriptionId,
        } as ExtensionToWebviewMessage);
        let buffer = '';
        response.setEncoding('utf-8');
        response.on('data', (chunk: string) => {
          if (!isActiveSubscription()) {
            return;
          }
          buffer += chunk;
          // SSE 协议：消息以双换行分隔
          const parts = buffer.split('\n\n');
          buffer = parts.pop() || '';
          for (const part of parts) {
            const trimmed = part.trim();
            if (!trimmed) continue;
            // 提取 data: 行
            for (const line of trimmed.split('\n')) {
              if (line.startsWith('data:')) {
                const data = line.slice(5).trim();
                if (data) {
                  this.postMessage({ type: 'agentSseEvent', data, subscriptionId } as ExtensionToWebviewMessage);
                }
              }
            }
          }
        });
        response.on('end', () => {
          if (this.activeSseRequest === request) {
            this.activeSseRequest = null;
          }
          if (!isActiveSubscription()) {
            return;
          }
          logger.info('界面.SSE代理.连接关闭', { subscriptionId }, LogCategory.UI);
          this.postMessage({
            type: 'agentSseStatus',
            status: 'error',
            subscriptionId,
          } as ExtensionToWebviewMessage);
        });
      },
    );
    request.on('error', (error) => {
      if (this.activeSseRequest === request) {
        this.activeSseRequest = null;
      }
      if (!isActiveSubscription()) {
        return;
      }
      logger.error('界面.SSE代理.请求错误', {
        error: error.message,
        subscriptionId,
      }, LogCategory.UI);
      this.postMessage({
        type: 'agentSseStatus',
        status: 'error',
        subscriptionId,
      } as ExtensionToWebviewMessage);
    });
    this.activeSseRequest = request;
  }

  private closeSseForwarding(subscriptionId?: string): void {
    if (subscriptionId && this.activeSseSubscriptionId !== subscriptionId) {
      return;
    }
    this.activeSseSubscriptionId = null;
    if (this.activeSseRequest) {
      this.activeSseRequest.destroy();
      this.activeSseRequest = null;
    }
  }

  private normalizeLocaleCode(locale: unknown): LocaleCode {
    return locale === 'en-US' ? 'en-US' : 'zh-CN';
  }

  private buildAgentQuery(extra?: Record<string, string>): string {
    const query = new URLSearchParams({
      workspaceId: this.workspaceId,
      workspacePath: this.workspaceRoot,
    });
    if (this.currentSessionId) {
      query.set('sessionId', this.currentSessionId);
    }
    for (const [key, value] of Object.entries(extra ?? {})) {
      const normalized = value.trim();
      if (normalized) {
        query.set(key, normalized);
      }
    }
    return query.toString();
  }

  private async fetchAgentJson<T>(pathname: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${await this.resolveActiveAgentBaseUrl()}${pathname}`, init);
    if (!response.ok) {
      throw new Error(`${pathname} failed: ${response.status}`);
    }
    return await response.json() as T;
  }

  private updateAgentBaseUrl(nextBaseUrl: string): void {
    const normalized = nextBaseUrl.trim();
    if (!normalized) {
      throw new Error('[WebviewProvider] Agent 地址为空');
    }
    if (normalized === this.agentBaseUrl) {
      return;
    }
    this.agentBaseUrl = normalized;
    logger.info('界面.Webview.Agent地址已刷新', {
      workspaceRoot: this.workspaceRoot,
      workspaceId: this.workspaceId,
      agentBaseUrl: this.agentBaseUrl,
    }, LogCategory.UI);
  }

  private async resolveActiveAgentBaseUrl(): Promise<string> {
    this.updateAgentBaseUrl(await this.agentController.ensureReadyBaseUrl());
    return this.agentBaseUrl;
  }

  private sendData(
    dataType: DataMessageType,
    payload: Record<string, unknown>,
    sessionId?: string | null,
  ): void {
    const resolvedSessionId = typeof sessionId === 'string' && sessionId.trim()
      ? sessionId.trim()
      : null;

    const message = createDataMessage(
      dataType,
      payload,
      resolvedSessionId || `shell:${dataType}`,
      resolvedSessionId ? { metadata: { sessionId: resolvedSessionId } } : undefined,
    );

    this.postMessage({
      type: 'unifiedMessage',
      message,
      sessionId: resolvedSessionId,
    });
  }

  private sendToast(message: string, level: NotifyLevel = 'info', duration?: number): void {
    const notifyMessage = createNotifyMessage(
      message,
      level,
      'shell:toast',
      duration,
      {
        displayMode: 'toast',
        category: 'feedback',
        source: 'ui-feedback',
      },
    );
    this.postMessage({ type: 'unifiedMessage', message: notifyMessage });
  }

  private postMessage(message: ExtensionToWebviewMessage): void {
    this.postQueue = this.postQueue
      .catch(() => undefined)
      .then(async () => {
        if (!this._view) {
          logger.warn('界面.Webview.未就绪_消息丢弃', { type: message.type }, LogCategory.UI);
          return;
        }
        await this._view.webview.postMessage(message);
      })
      .catch((error) => {
        logger.warn('界面.Webview.消息发送失败', {
          type: message.type,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.UI);
      });
  }

  private async handleOpenFile(message: Extract<WebviewToExtensionMessage, { type: 'openFile' }>): Promise<void> {
    const targetPath = this.resolveOpenFilePath(message);
    if (!targetPath) {
      this.sendToast(t('toast.openFileMissingPath'), 'warning');
      return;
    }

    this.syncCurrentSessionId(message.sessionId);

    try {
      const previewAbsolutePath = this.resolvePreviewAbsolutePath(
        message.previewAbsolutePath,
        message.previewCanOpenWorkspaceFile,
      );
      if (previewAbsolutePath) {
        const document = await vscode.workspace.openTextDocument(vscode.Uri.file(previewAbsolutePath));
        await vscode.window.showTextDocument(document, { preview: false, preserveFocus: false });
        return;
      }

      const directTarget = this.workspaceRoots.resolvePath(targetPath, { mustExist: false });
      if (directTarget?.absolutePath && fs.existsSync(directTarget.absolutePath)) {
        const stat = fs.statSync(directTarget.absolutePath);
        const uri = vscode.Uri.file(directTarget.absolutePath);
        if (stat.isDirectory()) {
          await vscode.commands.executeCommand('revealInExplorer', uri);
          return;
        }
        const document = await vscode.workspace.openTextDocument(uri);
        await vscode.window.showTextDocument(document, { preview: false, preserveFocus: false });
        return;
      }
    } catch {
      // 多工作区未显式前缀时直接回落到 Agent 侧解析，保持单链路解析权威。
    }

    try {
      if (typeof message.previewContent === 'string') {
        const previewPath = await this.createTempPreviewFile('file-preview', targetPath, message.previewContent);
        const document = await vscode.workspace.openTextDocument(vscode.Uri.file(previewPath));
        await vscode.window.showTextDocument(document, { preview: false, preserveFocus: false });
        return;
      }

      const query = this.buildAgentQuery({
        filePath: targetPath,
        sessionId: this.resolveRequestedSessionId(message.sessionId),
      });
      const payload = await this.fetchAgentJson<AgentFilePreviewPayload>(`/api/files/content?${query}`);
      if (payload.exists && payload.absolutePath && fs.existsSync(payload.absolutePath)) {
        const document = await vscode.workspace.openTextDocument(vscode.Uri.file(payload.absolutePath));
        await vscode.window.showTextDocument(document, { preview: false, preserveFocus: false });
        return;
      }
      const previewPath = await this.createTempPreviewFile('file-preview', payload.filePath, payload.content);
      const document = await vscode.workspace.openTextDocument(vscode.Uri.file(previewPath));
      await vscode.window.showTextDocument(document, { preview: false, preserveFocus: false });
      this.sendToast(t('toast.fileNotExists', { filepath: payload.filePath }), 'warning');
    } catch (error) {
      logger.error('界面.文件.打开_失败', {
        filePath: targetPath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.UI);
      this.sendToast(t('toast.openFileFailed', { filepath: targetPath }), 'error');
    }
  }

  private async handleOpenLink(url: string): Promise<void> {
    try {
      await vscode.env.openExternal(vscode.Uri.parse(url));
    } catch (error) {
      logger.error('界面.链接.打开_失败', {
        url,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.UI);
      this.sendToast('打开链接失败', 'error');
    }
  }

  private handleOpenMermaidPanel(code: string, title?: string): void {
    if (!code.trim()) {
      this.sendToast(t('toast.openChartFailed', { error: 'Mermaid code is empty' }), 'error');
      return;
    }

    try {
      MermaidPanel.createOrShow(this.extensionUri, code, title);
      logger.info('Mermaid.新标签页.已打开', { title }, LogCategory.UI);
    } catch (error) {
      logger.error('Mermaid.新标签页.打开失败', {
        title,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.UI);
      this.sendToast(
        t('toast.openChartFailed', { error: error instanceof Error ? error.message : String(error) }),
        'error',
      );
    }
  }

  private async openVscodeDiff(
    message: Extract<WebviewToExtensionMessage, { type: 'viewDiff' }>,
  ): Promise<void> {
    const normalizedFilePath = message.filePath.trim();
    if (!normalizedFilePath) {
      this.sendToast(t('toast.snapshotNotFound'), 'warning');
      return;
    }

    this.syncCurrentSessionId(message.sessionId);

    try {
      const inlineOriginalContent = typeof message.originalContent === 'string' ? message.originalContent : '';
      const inlineCurrentContent = typeof message.previewContent === 'string' ? message.previewContent : '';
      const inlineCurrentAbsolutePath = this.resolvePreviewAbsolutePath(
        message.previewAbsolutePath,
        message.previewCanOpenWorkspaceFile,
      );

      let payloadFilePath = normalizedFilePath;
      let originalContent = inlineOriginalContent;
      let currentContent = inlineCurrentContent;
      let currentUri: vscode.Uri | null = inlineCurrentAbsolutePath
        ? vscode.Uri.file(inlineCurrentAbsolutePath)
        : null;

      if (!originalContent && !currentUri && !currentContent) {
        const query = this.buildAgentQuery({
          filePath: normalizedFilePath,
          sessionId: this.resolveRequestedSessionId(message.sessionId),
        });
        const payload = await this.fetchAgentJson<AgentChangeDiffPayload>(`/api/changes/diff?${query}`);
        payloadFilePath = payload.filePath;
        originalContent = payload.originalContent || '';
        currentContent = payload.currentContent || '';

        const currentAbsolutePath = typeof payload.currentAbsolutePath === 'string'
          ? payload.currentAbsolutePath.trim()
          : '';
        const currentExists = payload.currentExists === true && currentAbsolutePath && fs.existsSync(currentAbsolutePath);
        currentUri = currentExists
          ? vscode.Uri.file(currentAbsolutePath)
          : null;
      }

      const originalUri = vscode.Uri.file(
        await this.createTempPreviewFile('diff-original', payloadFilePath, originalContent),
      );

      if (!currentUri) {
        currentUri = vscode.Uri.file(
          await this.createTempPreviewFile('diff-current', payloadFilePath, currentContent),
        );
      }

      const title = t('provider.diffTitle', { fileName: path.basename(payloadFilePath) });
      await vscode.commands.executeCommand('vscode.diff', originalUri, currentUri, title);
    } catch (error) {
      logger.error('界面.差异.打开_失败', {
        filePath: normalizedFilePath,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.UI);
      this.sendToast(t('toast.diffViewFailed'), 'error');
    }
  }

  private async createTempPreviewFile(prefix: string, relativePath: string, content: string): Promise<string> {
    const safeBaseName = path.basename(relativePath || 'preview.txt') || 'preview.txt';
    const tempDir = path.join(os.tmpdir(), 'magi-previews');
    fs.mkdirSync(tempDir, { recursive: true });

    const tempPath = path.join(
      tempDir,
      `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}-${safeBaseName}`,
    );
    fs.writeFileSync(tempPath, content, 'utf8');
    this.tempFiles.add(tempPath);
    return tempPath;
  }

  private resolveOpenFilePath(message: Extract<WebviewToExtensionMessage, { type: 'openFile' }>): string | null {
    const candidates = [message.filepath, message.filePath];
    for (const value of candidates) {
      if (typeof value === 'string' && value.trim()) {
        return value.trim();
      }
    }
    return null;
  }

  private syncCurrentSessionId(sessionId?: string): void {
    if (typeof sessionId !== 'string') {
      return;
    }
    const normalized = sessionId.trim();
    if (normalized) {
      this.currentSessionId = normalized;
    }
  }

  private resolveRequestedSessionId(sessionId?: string): string {
    if (typeof sessionId === 'string' && sessionId.trim()) {
      return sessionId.trim();
    }
    return this.currentSessionId;
  }

  private resolvePreviewAbsolutePath(
    previewAbsolutePath?: string,
    previewCanOpenWorkspaceFile?: boolean,
  ): string | null {
    if (previewCanOpenWorkspaceFile !== true) {
      return null;
    }
    if (typeof previewAbsolutePath !== 'string') {
      return null;
    }
    const normalized = previewAbsolutePath.trim();
    if (!normalized || !fs.existsSync(normalized)) {
      return null;
    }
    return normalized;
  }

  private getHtmlContent(webview: vscode.Webview): string {
    const templatePath = path.join(this.extensionUri.fsPath, 'dist', 'webview', 'index.html');
    if (!fs.existsSync(templatePath)) {
      const message = t('provider.errors.svelteWebviewNotBuilt', { templatePath });
      logger.error('界面.Svelte.未构建', { path: templatePath }, LogCategory.UI);
      throw new Error(message);
    }

    let html = fs.readFileSync(templatePath, 'utf-8');
    const cacheBuster = Date.now().toString();
    const webviewAssetsUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.extensionUri.fsPath, 'dist', 'webview', 'assets')),
    );

    html = html.replace(/src="\.\/assets\//g, `src="${webviewAssetsUri}/`);
    html = html.replace(/href="\.\/assets\//g, `href="${webviewAssetsUri}/`);
    html = html.replace(/\.js"/g, `.js?v=${cacheBuster}"`);
    html = html.replace(/\.css"/g, `.css?v=${cacheBuster}"`);

    const nonce = getNonce();
    // WebView 不直连 Agent（所有请求走 postMessage 代理），CSP 无需 agentConnectSrc
    const cspMeta = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}' ${webview.cspSource}; font-src ${webview.cspSource}; img-src ${webview.cspSource} https: data:; connect-src ${webview.cspSource};">`;
    html = html.replace('<head>', `<head>\n    ${cspMeta}`);

    const bootstrapScript = `<script nonce="${nonce}">window.__INITIAL_SESSION_ID__ = ${JSON.stringify('')}; window.__INITIAL_LOCALE__ = ${JSON.stringify(this.locale)}; window.__AGENT_BASE_URL__ = ${JSON.stringify(this.agentBaseUrl)}; window.__INITIAL_WORKSPACE_ID__ = ${JSON.stringify(this.workspaceId)}; window.__INITIAL_WORKSPACE_PATH__ = ${JSON.stringify(this.workspaceRoot)};</script>`;
    html = html.replace('</head>', `${bootstrapScript}\n  </head>`);

    logger.debug('界面.Svelte.Agent壳已注入', {
      locale: this.locale,
      workspaceRoot: this.workspaceRoot,
      workspaceId: this.workspaceId,
      agentBaseUrl: this.agentBaseUrl,
    }, LogCategory.UI);
    return html;
  }

  private getWebviewErrorHtml(errorMessage: string): string {
    const title = this.escapeHtml(t('provider.errors.webviewRenderTitle'));
    const hint = this.escapeHtml(t('provider.errors.webviewRenderHint'));
    const details = this.escapeHtml(errorMessage);
    return `<!DOCTYPE html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>${title}</title>
    <style>
      body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; padding: 24px; color: #1f2328; }
      .title { font-size: 18px; font-weight: 600; margin-bottom: 8px; }
      .hint { font-size: 13px; color: #636c76; margin-bottom: 12px; }
      .details { font-size: 12px; background: #f6f8fa; padding: 12px; border-radius: 8px; white-space: pre-wrap; }
    </style>
  </head>
  <body>
    <div class="title">${title}</div>
    <div class="hint">${hint}</div>
    <div class="details">${details}</div>
  </body>
</html>`;
  }

  private escapeHtml(input: string): string {
    return input
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
  }
}
