/**
 * WorkerStatusService - Worker 模型连接状态检查服务
 *
 * 从 WebviewProvider 提取的独立模块（#18 WVP 瘦身）。
 * 职责：
 * - 检查所有 Worker / Orchestrator / Compressor 模型连接状态
 * - 维护状态缓存（TTL + soft TTL）
 * - 向前端推送 workerStatusUpdate 数据事件
 */

import { logger, LogCategory } from '../logging';
import type { WorkerSlot } from '../types';
import type { AgentType } from '../types/agent-types';
import type { DataMessageType } from '../protocol/message-protocol';
import type { IAdapterFactory } from '../adapters/adapter-factory-interface';

// ============================================================================
// 上下文接口 - WorkerStatusService 对 WVP 的依赖声明
// ============================================================================

export interface WorkerStatusContext {
  sendData(dataType: DataMessageType, payload: Record<string, unknown>): void;
  getAdapterFactory(): IAdapterFactory;
}

// ============================================================================
// WorkerStatusService
// ============================================================================

export class WorkerStatusService {
  private workerStatusCache: Record<string, { status: string; model?: string; error?: string }> | null = null;
  private workerStatusCacheAt = 0;
  private workerStatusInFlight: Promise<void> | null = null;
  private readonly workerStatusCacheTtlMs = 30000;
  private readonly workerStatusSoftTtlMs = 120000;

  constructor(private readonly ctx: WorkerStatusContext) {}

  async sendWorkerStatus(force: boolean = false): Promise<void> {
    try {
      const now = Date.now();
      if (!force && this.workerStatusCache && (now - this.workerStatusCacheAt) < this.workerStatusCacheTtlMs) {
        this.ctx.sendData('workerStatusUpdate', { statuses: this.workerStatusCache });
        return;
      }

      if (this.workerStatusInFlight) {
        if (!force) {
          return;
        }
        await this.workerStatusInFlight;
      }

      const runCheck = this.performWorkerStatusCheck(force);
      this.workerStatusInFlight = runCheck;
      await runCheck;
    } catch (error: any) {
      logger.error('界面.模型状态.检查_失败', { error: error.message }, LogCategory.UI);
    } finally {
      this.workerStatusInFlight = null;
    }
  }

  private async performWorkerStatusCheck(force: boolean): Promise<void> {
    const { LLMConfigLoader } = await import('../llm/config');
    const { getOrCreateLLMClient } = await import('../llm/clients/client-factory');

    const config = LLMConfigLoader.loadFullConfig();
    const statuses: Record<string, { status: string; model?: string; error?: string }> = {};
    const now = Date.now();
    const priorityModels: Array<'orchestrator' | 'compressor'> = ['orchestrator', 'compressor'];
    const workerModels: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    const modelIds = [...priorityModels, ...workerModels];
    const formatModelLabel = (modelConfig: any): string | undefined => {
      if (!modelConfig?.provider || !modelConfig?.model) return undefined;
      return `${modelConfig.provider} - ${modelConfig.model}`;
    };
    const orchestratorLabel = formatModelLabel(config.orchestrator);
    const compressorFallbackLabel = orchestratorLabel ? `编排模型: ${orchestratorLabel}` : '编排模型';
    const setCompressorFallback = (reason: string) => {
      statuses.compressor = { status: 'fallback', model: compressorFallbackLabel, error: reason };
    };

    const getCachedStatus = (name: string) => {
      if (!this.workerStatusCache) return null;
      if ((now - this.workerStatusCacheAt) > this.workerStatusSoftTtlMs) return null;
      return this.workerStatusCache[name] || null;
    };

    const applyQuickStatus = (name: string, modelLabel?: string) => {
      const cached = getCachedStatus(name);
      if (cached) {
        statuses[name] = {
          status: cached.status,
          model: cached.model || modelLabel,
          error: cached.error
        };
        return true;
      }
      return false;
    };

    const adapterFactory = this.ctx.getAdapterFactory();

    // 测试模型的通用函数（使用快速 Models API）
    const testModel = async (name: string, modelConfig: any, isRequired: boolean = false) => {
      const isCompressor = name === 'compressor';
      if (!modelConfig.enabled || !modelConfig.apiKey || !modelConfig.model) {
        if (isCompressor) {
          setCompressorFallback(!modelConfig.enabled ? '压缩模型未启用' : '压缩模型未配置');
          return;
        }
        if (!modelConfig.enabled) {
          statuses[name] = {
            status: 'disabled',
            model: '已禁用'
          };
          return;
        }
        statuses[name] = {
          status: 'not_configured',
          model: isRequired ? '未配置（必需）' : '未配置'
        };
        return;
      }

      const modelLabel = formatModelLabel(modelConfig) || '未配置';
      if (!force) {
        const isConnected = name !== 'compressor'
          && adapterFactory.isConnected(name as AgentType);
        if (isConnected) {
          statuses[name] = { status: 'available', model: modelLabel };
          return;
        }
        if (applyQuickStatus(name, modelLabel)) {
          return;
        }
      }

      try {
        statuses[name] = {
          status: 'checking',
          model: modelLabel
        };

        // 使用快速连接测试（Models API）
        const client = getOrCreateLLMClient(modelConfig);
        const result = await client.testConnectionFast();

        if (result.success) {
          // 检查模型是否存在（如果 API 支持）
          if (result.modelExists === false) {
            if (isCompressor) {
              setCompressorFallback(`模型不存在: ${modelConfig.model}`);
              return;
            }
            statuses[name] = { status: 'invalid_model', model: modelLabel, error: `模型不存在: ${modelConfig.model}` };
          } else {
            statuses[name] = {
              status: 'available',
              model: modelLabel
            };
          }
        } else {
          if (isCompressor) {
            setCompressorFallback(result.error || '压缩模型连接失败');
            return;
          }
          // 根据错误类型设置状态
          let status = 'error';
          if (result.error?.includes('API Key')) {
            status = 'auth_failed';
          } else if (result.error?.includes('网络') || result.error?.includes('连接')) {
            status = 'network_error';
          } else if (result.error?.includes('超时')) {
            status = 'timeout';
          }

          statuses[name] = {
            status,
            model: modelLabel,
            error: result.error
          };
        }

        logger.info(`Model connection test (fast): ${name}`, {
          provider: modelConfig.provider,
          model: modelConfig.model,
          success: result.success,
          modelExists: result.modelExists,
        }, LogCategory.LLM);
      } catch (error: any) {
        if (isCompressor) {
          setCompressorFallback(error.message || '压缩模型连接失败');
          return;
        }
        statuses[name] = { status: 'error', model: modelLabel, error: error.message };

        logger.warn(`Model connection test failed: ${name}`, {
          error: error.message
        }, LogCategory.LLM);
      }
    };

    // 初始化占位状态，确保 UI 先显示检测中/缓存结果
    modelIds.forEach(name => {
      const modelConfig = name === 'orchestrator'
        ? config.orchestrator
        : name === 'compressor'
          ? config.compressor
          : config.workers[name as WorkerSlot];

      if (name === 'compressor' && (!modelConfig?.enabled || !modelConfig?.apiKey || !modelConfig?.model)) {
        setCompressorFallback(!modelConfig?.enabled ? '压缩模型未启用' : '压缩模型未配置');
        return;
      }

      if (!modelConfig?.enabled) {
        statuses[name] = { status: 'disabled', model: '已禁用' };
        return;
      }
      if (!modelConfig?.apiKey || !modelConfig?.model) {
        statuses[name] = {
          status: 'not_configured',
          model: name === 'orchestrator' || name === 'compressor' ? '未配置（必需）' : '未配置'
        };
        return;
      }

      const modelLabel = formatModelLabel(modelConfig) || '未配置';
      if (!force) {
        const isConnected = name !== 'compressor'
          && adapterFactory.isConnected(name as AgentType);
        if (isConnected) {
          statuses[name] = { status: 'available', model: modelLabel };
          return;
        }
        if (applyQuickStatus(name, modelLabel)) {
          return;
        }
      }
      statuses[name] = { status: 'checking', model: modelLabel };
    });

    this.ctx.sendData('workerStatusUpdate', { statuses });

    // 所有模型并行检测（不再串行）
    await Promise.all([
      testModel('orchestrator', config.orchestrator, true),
      testModel('compressor', config.compressor, true),
      ...workerModels.map(worker => testModel(worker, config.workers[worker]))
    ]);

    this.workerStatusCache = statuses;
    this.workerStatusCacheAt = Date.now();

    this.ctx.sendData('workerStatusUpdate', { statuses });

    logger.info('Model connection status check completed', {
      results: Object.entries(statuses).map(([name, s]) => `${name}: ${s.status}`),
      mode: force ? 'hard' : 'soft'
    }, LogCategory.LLM);
  }
}
