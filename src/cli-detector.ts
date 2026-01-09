/**
 * CLI 检测器模块
 * 负责检测各 CLI 工具的可用性并制定降级策略
 * 版本: 0.3.0 - 添加健康检查和事件发射
 */

import { exec } from 'child_process';
import { promisify } from 'util';
import * as vscode from 'vscode';
import {
  CLIType,
  CLIStatus,
  CLIStatusCode,
  DegradationLevel,
  DegradationStrategy,
  TaskCategory,
  CLI_ROLES
} from './types';
import { globalEventBus } from './events';

const execAsync = promisify(exec);

// CLI 版本检测命令
const VERSION_COMMANDS: Record<CLIType, string> = {
  claude: '--version',
  codex: '--version',
  gemini: '--version'
};

// CLI 最低版本要求
const MIN_VERSIONS: Record<CLIType, string> = {
  claude: '2.0.0',
  codex: '0.1.0',
  gemini: '0.1.0'
};

export class CLIDetector {
  private config: vscode.WorkspaceConfiguration;
  private statusCache: Map<CLIType, CLIStatus> = new Map();
  private cacheExpiry: number = 5 * 60 * 1000; // 5分钟缓存
  private lastCheck: number = 0;
  private healthCheckInterval: NodeJS.Timeout | null = null;
  private healthCheckPeriod: number = 60 * 1000; // 1分钟健康检查

  constructor() {
    this.config = vscode.workspace.getConfiguration('cliArranger');
  }

  /**
   * 启动健康检查定时器
   */
  startHealthCheck(): void {
    if (this.healthCheckInterval) {
      return;
    }
    this.healthCheckInterval = setInterval(async () => {
      const statuses = await this.checkAllCLIs(true);
      globalEventBus.emitEvent('cli:healthCheck', {
        data: { statuses, timestamp: Date.now() }
      });
    }, this.healthCheckPeriod);
  }

  /**
   * 停止健康检查定时器
   */
  stopHealthCheck(): void {
    if (this.healthCheckInterval) {
      clearInterval(this.healthCheckInterval);
      this.healthCheckInterval = null;
    }
  }

  /**
   * 获取 CLI 路径配置
   */
  private getCLIPath(type: CLIType): string {
    return this.config.get<string>(`${type}.path`, type);
  }

  /**
   * 检测单个 CLI 的可用性 (支持更细粒度状态)
   */
  async checkCLI(type: CLIType): Promise<CLIStatus> {
    const path = this.getCLIPath(type);
    const command = `${path} ${VERSION_COMMANDS[type]}`;
    const previousStatus = this.statusCache.get(type);

    try {
      const { stdout } = await execAsync(command, { timeout: 10000 });
      const version = this.parseVersion(stdout);

      const status: CLIStatus = {
        type,
        code: CLIStatusCode.AVAILABLE,
        available: true,
        version,
        path,
        lastChecked: new Date()
      };

      this.statusCache.set(type, status);

      // 状态变化时发射事件
      if (previousStatus?.code !== status.code) {
        globalEventBus.emitEvent('cli:statusChanged', {
          data: { cli: type, previousCode: previousStatus?.code, newCode: status.code, status }
        });
      }

      return status;
    } catch (error) {
      const { code, errorMsg } = this.parseError(error, type);

      const status: CLIStatus = {
        type,
        code,
        available: false,
        path,
        error: errorMsg,
        lastChecked: new Date()
      };

      this.statusCache.set(type, status);

      // 状态变化时发射事件
      if (previousStatus?.code !== status.code) {
        globalEventBus.emitEvent('cli:statusChanged', {
          data: { cli: type, previousCode: previousStatus?.code, newCode: status.code, status }
        });
        // 错误时额外发射错误事件
        globalEventBus.emitEvent('cli:error', {
          data: { cli: type, code, error: errorMsg }
        });
      }

      return status;
    }
  }

  /**
   * 检测所有 CLI
   */
  async checkAllCLIs(forceRefresh = false): Promise<CLIStatus[]> {
    const now = Date.now();

    // 使用缓存
    if (!forceRefresh && now - this.lastCheck < this.cacheExpiry) {
      const cached = Array.from(this.statusCache.values());
      if (cached.length === 3) {
        return cached;
      }
    }

    const types: CLIType[] = ['claude', 'codex', 'gemini'];
    const results = await Promise.all(types.map(t => this.checkCLI(t)));

    this.lastCheck = now;
    return results;
  }

  /**
   * 制定降级策略 (包含能力分配)
   */
  async getDegradationStrategy(): Promise<DegradationStrategy> {
    const statuses = await this.checkAllCLIs();
    const available = statuses.filter(s => s.available).map(s => s.type);
    const missing = statuses.filter(s => !s.available).map(s => s.type);

    const hasClaude = available.includes('claude');
    const hasCodex = available.includes('codex');
    const hasGemini = available.includes('gemini');

    // 根据可用 CLI 组合确定降级策略
    const strategy = this.buildStrategy(available, missing, hasClaude, hasCodex, hasGemini);

    return strategy;
  }

  /**
   * 构建降级策略 (核心逻辑)
   */
  private buildStrategy(
    available: CLIType[],
    missing: CLIType[],
    hasClaude: boolean,
    hasCodex: boolean,
    hasGemini: boolean
  ): DegradationStrategy {

    // 模式 1: 全功能 (Claude + Codex + Gemini)
    if (hasClaude && hasCodex && hasGemini) {
      return {
        level: DegradationLevel.FULL,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: true,
        recommendation: '✅ 全功能模式：Claude(编排) + Codex(Bug修复) + Gemini(前端)',
        canProceed: true,
        fallbackMap: {}
      };
    }

    // 模式 2: Claude + Codex (Gemini 缺失)
    if (hasClaude && hasCodex && !hasGemini) {
      return {
        level: DegradationLevel.DUAL,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: true,
        recommendation: '⚡ Claude + Codex 模式：前端任务由 Claude 降级处理',
        canProceed: true,
        fallbackMap: { gemini: 'claude' }
      };
    }

    // 模式 3: Claude + Gemini (Codex 缺失)
    if (hasClaude && !hasCodex && hasGemini) {
      return {
        level: DegradationLevel.DUAL,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: true,
        recommendation: '⚡ Claude + Gemini 模式：Bug修复由 Claude 降级处理',
        canProceed: true,
        fallbackMap: { codex: 'claude' }
      };
    }

    // 模式 4: 仅 Claude
    if (hasClaude && !hasCodex && !hasGemini) {
      return {
        level: DegradationLevel.SINGLE_CLAUDE,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: true,
        recommendation: '⚠️ 仅 Claude 模式：所有任务由 Claude 独立完成',
        canProceed: true,
        fallbackMap: { codex: 'claude', gemini: 'claude' }
      };
    }

    // 模式 5: Codex + Gemini (Claude 缺失) - 简单模式
    if (!hasClaude && hasCodex && hasGemini) {
      return {
        level: DegradationLevel.SINGLE_OTHER,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: false,
        recommendation: '⚠️ 简单模式：无智能编排，仅基于关键词分配任务',
        canProceed: true,
        fallbackMap: { claude: 'codex' }  // 架构任务勉强由 Codex 处理
      };
    }

    // 模式 6: 仅 Codex
    if (!hasClaude && hasCodex && !hasGemini) {
      return {
        level: DegradationLevel.SINGLE_OTHER,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: false,
        recommendation: '⚠️ 仅 Codex 模式：仅适合 Bug 修复类任务',
        canProceed: true,
        fallbackMap: { claude: 'codex', gemini: 'codex' }
      };
    }

    // 模式 7: 仅 Gemini
    if (!hasClaude && !hasCodex && hasGemini) {
      return {
        level: DegradationLevel.SINGLE_OTHER,
        availableCLIs: available,
        missingCLIs: missing,
        hasOrchestrator: false,
        recommendation: '⚠️ 仅 Gemini 模式：仅适合前端类任务',
        canProceed: true,
        fallbackMap: { claude: 'gemini', codex: 'gemini' }
      };
    }

    // 模式 8: 无可用 CLI
    return {
      level: DegradationLevel.NONE,
      availableCLIs: available,
      missingCLIs: missing,
      hasOrchestrator: false,
      recommendation: '❌ 无可用 CLI，请至少安装一个 CLI 工具',
      canProceed: false,
      fallbackMap: {}
    };
  }

  /**
   * 获取任务的最佳处理 CLI
   */
  getHandlerForTask(taskType: TaskCategory, strategy: DegradationStrategy): CLIType | null {
    if (!strategy.canProceed) {
      return null;
    }

    // 找到最适合该任务类型的 CLI
    const preferredCLI = (Object.entries(CLI_ROLES) as [CLIType, typeof CLI_ROLES[CLIType]][])
      .filter(([type]) => strategy.availableCLIs.includes(type))
      .sort((a, b) => a[1].priority - b[1].priority)
      .find(([, role]) => role.taskAffinity.includes(taskType));

    if (preferredCLI) {
      return preferredCLI[0];
    }

    // 如果没有找到专门的，使用降级映射或第一个可用的
    return strategy.availableCLIs[0] || null;
  }

  /**
   * 解析版本号
   */
  private parseVersion(output: string): string {
    const match = output.match(/(\d+\.\d+\.\d+)/);
    return match ? match[1] : 'unknown';
  }

  /**
   * 比较版本号
   * @returns -1 if v1 < v2, 0 if equal, 1 if v1 > v2
   */
  private compareVersions(v1: string, v2: string): number {
    if (v1 === 'unknown' || v2 === 'unknown') return 0;
    const parts1 = v1.split('.').map(Number);
    const parts2 = v2.split('.').map(Number);
    for (let i = 0; i < 3; i++) {
      if (parts1[i] > parts2[i]) return 1;
      if (parts1[i] < parts2[i]) return -1;
    }
    return 0;
  }

  /**
   * 检查版本是否满足最低要求
   */
  checkVersionRequirement(type: CLIType, version: string): { meets: boolean; minVersion: string } {
    const minVersion = MIN_VERSIONS[type];
    const meets = this.compareVersions(version, minVersion) >= 0;
    return { meets, minVersion };
  }

  /**
   * 获取所有 CLI 的详细状态摘要
   */
  async getStatusSummary(): Promise<{
    available: number;
    total: number;
    statuses: CLIStatus[];
    recommendation: string;
  }> {
    const statuses = await this.checkAllCLIs();
    const available = statuses.filter(s => s.available).length;
    const strategy = await this.getDegradationStrategy();
    return {
      available,
      total: 3,
      statuses,
      recommendation: strategy.recommendation
    };
  }

  /**
   * 解析错误类型 (更细粒度)
   */
  private parseError(error: unknown, type: CLIType): { code: CLIStatusCode; errorMsg: string } {
    if (error instanceof Error) {
      const msg = error.message.toLowerCase();

      if (msg.includes('enoent') || msg.includes('not found') || msg.includes('command not found')) {
        return {
          code: CLIStatusCode.NOT_INSTALLED,
          errorMsg: `${type} CLI 未安装。${this.getInstallGuide(type)}`
        };
      }
      if (msg.includes('etimedout') || msg.includes('timeout')) {
        return {
          code: CLIStatusCode.TIMEOUT,
          errorMsg: `${type} CLI 响应超时，请检查网络连接`
        };
      }
      if (msg.includes('unauthorized') || msg.includes('401') || msg.includes('api key')) {
        return {
          code: CLIStatusCode.AUTH_FAILED,
          errorMsg: `${type} CLI 认证失败，请检查 API Key 配置`
        };
      }
      if (msg.includes('quota') || msg.includes('rate limit') || msg.includes('429')) {
        return {
          code: CLIStatusCode.QUOTA_EXCEEDED,
          errorMsg: `${type} CLI 配额耗尽，请稍后重试`
        };
      }
      if (msg.includes('network') || msg.includes('econnrefused') || msg.includes('enotfound')) {
        return {
          code: CLIStatusCode.NETWORK_ERROR,
          errorMsg: `${type} CLI 网络错误，请检查网络连接`
        };
      }

      return {
        code: CLIStatusCode.RUNTIME_ERROR,
        errorMsg: error.message
      };
    }

    return {
      code: CLIStatusCode.RUNTIME_ERROR,
      errorMsg: '未知错误'
    };
  }

  /**
   * 获取安装指引
   */
  getInstallGuide(type: CLIType): string {
    const guides: Record<CLIType, string> = {
      claude: 'npm install -g @anthropic-ai/claude-code',
      codex: 'npm install -g @openai/codex',
      gemini: 'npm install -g @google/gemini-cli'
    };
    return guides[type];
  }

  /**
   * 刷新配置
   */
  refreshConfig(): void {
    this.config = vscode.workspace.getConfiguration('cliArranger');
    this.statusCache.clear();
    this.lastCheck = 0;
  }
}

export const cliDetector = new CLIDetector();

