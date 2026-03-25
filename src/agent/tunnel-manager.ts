/**
 * Cloudflare Tunnel 管理器
 *
 * 职责：
 * 1. 检测/安装 cloudflared 二进制文件
 * 2. 启动/停止 Quick Tunnel（免费，无需账号）
 * 3. 生成一次性访问 token
 * 4. 解析隧道公网 URL
 */

import { ChildProcess, spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as crypto from 'crypto';
import * as https from 'https';
import { buildAgentWebClientUrl, type AgentWebClientBinding } from '../shared/agent-shared-config';

// ============================================================================
// 类型定义
// ============================================================================

export type TunnelStatus = 'stopped' | 'starting' | 'running' | 'stopping' | 'installing' | 'error';

export interface TunnelState {
  status: TunnelStatus;
  publicUrl: string | null;
  accessUrl: string | null;
  token: string | null;
  error: string | null;
}

// ============================================================================
// 常量
// ============================================================================

const MAGI_BIN_DIR = path.join(os.homedir(), '.magi', 'bin');

const DOWNLOAD_URLS: Record<string, Record<string, string>> = {
  darwin: {
    arm64: 'https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-arm64.tgz',
    x64: 'https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz',
  },
  linux: {
    arm64: 'https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64',
    x64: 'https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64',
  },
  win32: {
    x64: 'https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe',
  },
};

// ============================================================================
// TunnelManager 类
// ============================================================================

export class TunnelManager {
  private process: ChildProcess | null = null;
  private _status: TunnelStatus = 'stopped';
  private _publicUrl: string | null = null;
  private _token: string | null = null;
  private _error: string | null = null;
  private _binding: AgentWebClientBinding | undefined;
  private _localPort: number;
  private _onStateChange: ((state: TunnelState) => void) | null = null;

  constructor(localPort: number) {
    this._localPort = localPort;
  }

  /** 注册状态变化回调 */
  onStateChange(cb: (state: TunnelState) => void): void {
    this._onStateChange = cb;
  }

  /** 获取当前完整状态 */
  getState(): TunnelState {
    return {
      status: this._status,
      publicUrl: this._publicUrl,
      accessUrl: this._buildAccessUrl(),
      token: this._token,
      error: this._error,
    };
  }

  /** 启动隧道 */
  async start(binding?: AgentWebClientBinding): Promise<TunnelState> {
    if (this._status === 'running' || this._status === 'starting') {
      return this.getState();
    }

    this._binding = binding;
    this._error = null;

    // 1. 确保 cloudflared 可用
    let binPath = await this.resolveCloudflaredPath();
    if (!binPath) {
      this.setStatus('installing');
      try {
        binPath = await this.installCloudflared();
      } catch (err) {
        this._error = `安装 cloudflared 失败: ${err instanceof Error ? err.message : String(err)}`;
        this.setStatus('error');
        return this.getState();
      }
    }

    // 2. 生成 token
    this._token = crypto.randomBytes(24).toString('hex');

    // 3. 启动隧道
    this.setStatus('starting');
    try {
      await this.spawnTunnel(binPath);
    } catch (err) {
      this._error = `启动隧道失败: ${err instanceof Error ? err.message : String(err)}`;
      this._token = null;
      this.setStatus('error');
    }

    return this.getState();
  }

  /** 停止隧道 */
  async stop(): Promise<TunnelState> {
    if (this._status === 'stopped') return this.getState();
    this.setStatus('stopping');
    if (this.process) {
      this.process.kill('SIGTERM');
      // 给 2 秒优雅关闭，否则强杀
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          this.process?.kill('SIGKILL');
          resolve();
        }, 2000);
        this.process?.once('exit', () => { clearTimeout(timer); resolve(); });
      });
      this.process = null;
    }
    this._publicUrl = null;
    this._token = null;
    this._error = null;
    this.setStatus('stopped');
    return this.getState();
  }

  updateBinding(binding?: AgentWebClientBinding): TunnelState {
    if (binding) {
      this._binding = binding;
    }
    return this.getState();
  }

  validateToken(token: string | null | undefined): boolean {
    return Boolean(this._token && token && this._token === token);
  }

  async dispose(): Promise<void> {
    await this.stop();
  }


  // ==========================================================================
  // 内部方法
  // ==========================================================================

  private setStatus(status: TunnelStatus): void {
    this._status = status;
    this._onStateChange?.(this.getState());
  }

  private _buildAccessUrl(): string | null {
    if (!this._publicUrl || !this._token) return null;
    const url = buildAgentWebClientUrl(this._publicUrl, this._binding);
    const sep = url.includes('?') ? '&' : '?';
    return `${url}${sep}tunnel_token=${this._token}`;
  }

  /** 检测 cloudflared 可执行文件路径 */
  private async resolveCloudflaredPath(): Promise<string | null> {
    // 1. 检查 ~/.magi/bin/
    const localBin = path.join(MAGI_BIN_DIR, process.platform === 'win32' ? 'cloudflared.exe' : 'cloudflared');
    if (fs.existsSync(localBin)) return localBin;

    // 2. 检查 PATH
    const which = process.platform === 'win32' ? 'where' : 'which';
    try {
      const result = await new Promise<string>((resolve, reject) => {
        const proc = spawn(which, ['cloudflared'], { stdio: ['ignore', 'pipe', 'ignore'] });
        let out = '';
        proc.stdout.on('data', (d) => { out += d.toString(); });
        proc.on('close', (code) => code === 0 ? resolve(out.trim().split('\n')[0]) : reject());
        proc.on('error', reject);
      });
      if (result && fs.existsSync(result)) return result;
    } catch {
      // 不在 PATH 中
    }
    return null;
  }

  /** 下载并安装 cloudflared */
  private async installCloudflared(): Promise<string> {
    const platform = process.platform;
    const arch = process.arch;
    const urls = DOWNLOAD_URLS[platform];
    if (!urls) throw new Error(`不支持的平台: ${platform}`);
    const url = urls[arch];
    if (!url) throw new Error(`不支持的架构: ${platform}/${arch}`);

    fs.mkdirSync(MAGI_BIN_DIR, { recursive: true });
    const destBin = path.join(MAGI_BIN_DIR, platform === 'win32' ? 'cloudflared.exe' : 'cloudflared');
    const isTgz = url.endsWith('.tgz');

    if (isTgz) {
      // macOS: 下载 tgz 并解压
      const tgzPath = path.join(MAGI_BIN_DIR, 'cloudflared.tgz');
      await this.downloadFile(url, tgzPath);
      await new Promise<void>((resolve, reject) => {
        const proc = spawn('tar', ['-xzf', tgzPath, '-C', MAGI_BIN_DIR], { stdio: 'ignore' });
        proc.on('close', (code) => code === 0 ? resolve() : reject(new Error(`tar 解压失败 (code ${code})`)));
        proc.on('error', reject);
      });
      fs.rmSync(tgzPath, { force: true });
    } else {
      // Linux/Windows: 直接下载二进制
      await this.downloadFile(url, destBin);
    }

    // chmod +x
    if (platform !== 'win32') {
      fs.chmodSync(destBin, 0o755);
    }

    if (!fs.existsSync(destBin)) {
      throw new Error('安装完成但未找到 cloudflared 二进制文件');
    }
    return destBin;
  }

  /** 通过 HTTPS 下载文件（跟随重定向） */
  private downloadFile(url: string, dest: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const doRequest = (targetUrl: string, redirectCount = 0) => {
        if (redirectCount > 5) { reject(new Error('下载重定向次数过多')); return; }
        const mod = targetUrl.startsWith('https') ? https : require('http');
        mod.get(targetUrl, (res: { statusCode?: number; headers: { location?: string }; pipe: (s: fs.WriteStream) => void; on: (e: string, cb: () => void) => void; resume: () => void }) => {
          if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
            res.resume();
            doRequest(res.headers.location, redirectCount + 1);
            return;
          }
          if (res.statusCode && res.statusCode !== 200) {
            reject(new Error(`下载失败 HTTP ${res.statusCode}`));
            return;
          }
          const file = fs.createWriteStream(dest);
          res.pipe(file);
          file.on('finish', () => { file.close(); resolve(); });
          file.on('error', (err: Error) => { fs.rmSync(dest, { force: true }); reject(err); });
        }).on('error', reject);
      };
      doRequest(url);
    });
  }

  /** 启动 cloudflared quick tunnel 子进程 */
  private spawnTunnel(binPath: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const proc = spawn(binPath, ['tunnel', '--url', `http://127.0.0.1:${this._localPort}`], {
        stdio: ['ignore', 'pipe', 'pipe'],
        detached: false,
      });

      this.process = proc;
      let resolved = false;

      // 设置超时：30 秒内必须拿到公网 URL
      const timeout = setTimeout(() => {
        if (!resolved) {
          resolved = true;
          reject(new Error('等待隧道 URL 超时（30s）'));
          proc.kill('SIGTERM');
        }
      }, 30_000);

      const handleData = (chunk: Buffer) => {
        const line = chunk.toString();
        // cloudflared 输出格式: ... https://xxx-xxx.trycloudflare.com ...
        const match = line.match(/https:\/\/[a-zA-Z0-9-]+\.trycloudflare\.com/);
        if (match && !resolved) {
          resolved = true;
          clearTimeout(timeout);
          this._publicUrl = match[0];
          this.setStatus('running');
          resolve();
        }
      };

      proc.stdout?.on('data', handleData);
      proc.stderr?.on('data', handleData);

      proc.on('error', (err) => {
        clearTimeout(timeout);
        if (!resolved) { resolved = true; reject(err); }
      });

      proc.on('exit', (code) => {
        clearTimeout(timeout);
        if (this._status === 'running') {
          // 意外退出
          this._publicUrl = null;
          this._token = null;
          this._error = `隧道进程异常退出 (code ${code})`;
          this.setStatus('error');
        }
        if (!resolved) { resolved = true; reject(new Error(`cloudflared 退出 (code ${code})`)); }
        this.process = null;
      });
    });
  }
}

