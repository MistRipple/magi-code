/**
 * Worker LLM 适配器
 * 用于 Worker 代理（claude, codex, gemini）
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig, WorkerSlot } from '../../types/agent-types';
import {
  LLMClient,
  LLMMessageParams,
  LLMMessage,
  ToolCall,
  isSummaryHijackText,
  sanitizeSummaryHijackMessages,
  sanitizeToolOrder,
} from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';
import { t } from '../../i18n';
import { ProfileLoader } from '../../orchestrator/profile/profile-loader';
import { GuidanceInjector } from '../../orchestrator/profile/guidance-injector';
import { isRetryableNetworkError, toErrorMessage, deduplicateResumption } from '../../tools/network-utils';

/**
 * 历史管理配置
 */
export interface HistoryManagementConfig {
  /** 最大历史消息数量（默认 50） */
  maxMessages?: number;
  /** 最大历史字符数（默认 100000） */
  maxChars?: number;
  /** 保留最近 N 轮对话（默认 5） */
  preserveRecentRounds?: number;
}

/**
 * 停滞检测配置
 *
 * 按 Worker 模型特性差异化：
 *   - Codex 倾向大范围只读扫描，需要更早干预（参考 Augment 的 25 轮硬上限）
 *   - Claude 深度推理型，可以给更宽松的探索空间
 */
export interface StallDetectionConfig {
  /** 连续失败终止阈值 */
  consecutiveFailThreshold: number;
  /** 累计失败终止阈值 */
  totalFailLimit: number;
  /** 空转分数警告阈值：一级（温和建议） */
  stallWarnLevel1: number;
  /** 空转分数警告阈值：二级（明确要求） */
  stallWarnLevel2: number;
  /** 空转分数警告阈值：三级（最终警告） */
  stallWarnLevel3: number;
  /** 空转分数终止阈值 */
  stallAbortThreshold: number;
  /** 总轮次硬上限 */
  maxTotalRounds: number;
  /** 无实质输出一级提醒阈值 */
  noOutputWarn: number;
  /** 无实质输出强制产出阈值 */
  noOutputForce: number;
  /** 无实质输出终止阈值 */
  noOutputAbort: number;
}

/** 停滞检测预设：按 WorkerSlot 选择合适的阈值 */
const STALL_DETECTION_PRESETS: Record<WorkerSlot, StallDetectionConfig> = {
  claude: {
    consecutiveFailThreshold: 5,
    totalFailLimit: 25,
    stallWarnLevel1: 5,
    stallWarnLevel2: 10,
    stallWarnLevel3: 20,
    stallAbortThreshold: 30,
    maxTotalRounds: 45,
    noOutputWarn: 6,
    noOutputForce: 10,
    noOutputAbort: 15,
  },
  codex: {
    consecutiveFailThreshold: 5,
    totalFailLimit: 20,
    stallWarnLevel1: 4,
    stallWarnLevel2: 8,
    stallWarnLevel3: 14,
    stallAbortThreshold: 20,
    maxTotalRounds: 35,
    noOutputWarn: 6,
    noOutputForce: 10,
    noOutputAbort: 15,
  },
  gemini: {
    consecutiveFailThreshold: 5,
    totalFailLimit: 25,
    stallWarnLevel1: 5,
    stallWarnLevel2: 10,
    stallWarnLevel3: 20,
    stallAbortThreshold: 30,
    maxTotalRounds: 45,
    noOutputWarn: 6,
    noOutputForce: 10,
    noOutputAbort: 15,
  },
};

/** 获取指定 WorkerSlot 的停滞检测预设（返回副本，避免外部篡改） */
export function getStallDetectionPreset(workerSlot: WorkerSlot): StallDetectionConfig {
  return { ...STALL_DETECTION_PRESETS[workerSlot] };
}

/**
 * Worker 适配器配置
 */
export interface WorkerAdapterConfig {
  client: LLMClient;
  normalizer: BaseNormalizer;
  toolManager: ToolManager;
  config: LLMConfig;
  messageHub: MessageHub;  // 🔧 统一消息通道：替代 messageBus
  workerSlot: WorkerSlot;
  systemPrompt?: string;
  profileLoader: ProfileLoader;
  historyConfig?: HistoryManagementConfig;
  stallConfig?: StallDetectionConfig;
  executionPolicy?: {
    requestTimeoutMs?: number;
    retryPolicy?: {
      maxRetries?: number;
      baseDelayMs?: number;
      retryDelaysMs?: readonly number[];
      retryOnTimeout?: boolean;
      retryOnAllErrors?: boolean;
      maxRetryDurationMs?: number;
      deterministicErrorStreakLimit?: number;
    };
  };
}

/**
 * Worker LLM 适配器
 */
export class WorkerLLMAdapter extends BaseLLMAdapter {
  private static readonly DEFAULT_REQUEST_TIMEOUT_MS = 60_000;
  private static readonly DEFAULT_RETRY_POLICY = {
    maxRetries: 6, // 首次 + 5 次重试
    baseDelayMs: 500,
    retryDelaysMs: [10_000, 20_000, 30_000, 40_000, 50_000],
    retryOnTimeout: true,
    retryOnAllErrors: true,
    maxRetryDurationMs: 240_000,
    deterministicErrorStreakLimit: 3,
  } as const;
  /** 流式中断自动续跑预算（按一次 sendMessageInternal 计） */
  private static readonly STREAM_INTERRUPTION_RECOVERY_MAX = 2;
  private workerSlot: WorkerSlot;
  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private historyConfig: Required<HistoryManagementConfig>;
  private stallConfig: StallDetectionConfig;
  private readonly requestTimeoutMs: number;
  private readonly requestRetryPolicy: {
    maxRetries: number;
    baseDelayMs: number;
    retryDelaysMs?: readonly number[];
    retryOnTimeout: boolean;
    retryOnAllErrors?: boolean;
    maxRetryDurationMs?: number;
    deterministicErrorStreakLimit?: number;
  };
  private seenThinking = false;
  private decisionHookAppliedForThinking = false;
  /** 工具摘要是否已注入到 systemPrompt（lazy init，仅执行一次） */
  private toolsSummaryInjected = false;
  /** 写操作拦截缓存 TTL（避免环境修复后仍被旧记录阻断） */
  private static readonly FAILED_WRITE_CACHE_TTL_MS = 10 * 60 * 1000;
  private static readonly SUCCESS_WRITE_CACHE_TTL_MS = 10 * 60 * 1000;
  /** 只读工具去重缓存 TTL（避免长期缓存导致必要重读被误拦截） */
  private static readonly READ_ONLY_DEDUP_TTL_MS = 10 * 60 * 1000;
  private static readonly READ_ONLY_TOOL_NAMES = new Set<string>([
    'file_view',
    'grep_search',
    'web_search',
    'web_fetch',
    'mermaid_diagram',
    'codebase_retrieval',
    'read-process',
    'list-processes',
    'get_todos',
  ]);
  private static readonly WRITE_DEDUP_TOOL_NAMES = new Set<string>([
    'shell',
    'file_create',
    'file_edit',
    'file_insert',
    'file_bulk_edit',
    'file_remove',
  ]);
  /** 只读工具调用去重缓存（参数指纹） */
  private readOnlyCallCache = new Map<string, { count: number; firstAt: number; lastAt: number }>();
  /** 最近一次可能改写文件的时间戳（写工具或 shell 成功） */
  private lastMutationAt = 0;
  /**
   * 滚动上下文摘要：截断时从被丢弃消息中提取的关键信息
   *
   * 每次 truncateHistoryIfNeeded 触发截断时，被丢弃消息的精华会合并到此摘要中。
   * 此摘要以 user 角色消息注入到对话历史开头，确保 LLM 不丢失前期关键发现。
   */
  private rollingContextSummary: string | null = null;
  /** 滚动摘要最大字符数（约 500 tokens） */
  private static readonly MAX_ROLLING_SUMMARY_CHARS = 2000;
  /** 当前任务内的去重命中总次数（递增惩罚用） */
  private totalDedupHits = 0;
  /** 当前轮次的去重命中次数（空转分数加权用） */
  private roundDedupHits = 0;
  /** 失败写操作缓存：防止模型反复重试相同的失败写操作 */
  private failedWriteCache = new Map<string, { count: number; error: string; firstAt: number; lastAt: number }>();
  /** 成功写操作缓存：防止模型反复执行完全相同的已成功写操作 */
  private successWriteCache = new Map<string, { filePath: string; updatedAt: number }>();
  /** 当前轮次被去重拦截的写操作计数（用于空转检测判断是否有实际写入） */
  private roundWriteInterceptCount = 0;

  constructor(adapterConfig: WorkerAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageHub  // 🔧 统一消息通道：使用 messageHub
    );
    this.workerSlot = adapterConfig.workerSlot;
    this.profileLoader = adapterConfig.profileLoader;
    this.guidanceInjector = new GuidanceInjector();
    this.stallConfig = adapterConfig.stallConfig ?? getStallDetectionPreset(adapterConfig.workerSlot);
    this.systemPrompt = adapterConfig.systemPrompt || this.buildSystemPrompt();
    this.requestTimeoutMs = adapterConfig.executionPolicy?.requestTimeoutMs
      ?? WorkerLLMAdapter.DEFAULT_REQUEST_TIMEOUT_MS;
    this.requestRetryPolicy = {
      maxRetries: adapterConfig.executionPolicy?.retryPolicy?.maxRetries
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.maxRetries,
      baseDelayMs: adapterConfig.executionPolicy?.retryPolicy?.baseDelayMs
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.baseDelayMs,
      retryDelaysMs: adapterConfig.executionPolicy?.retryPolicy?.retryDelaysMs
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.retryDelaysMs,
      retryOnTimeout: adapterConfig.executionPolicy?.retryPolicy?.retryOnTimeout
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.retryOnTimeout,
      retryOnAllErrors: adapterConfig.executionPolicy?.retryPolicy?.retryOnAllErrors
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.retryOnAllErrors,
      maxRetryDurationMs: adapterConfig.executionPolicy?.retryPolicy?.maxRetryDurationMs
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.maxRetryDurationMs,
      deterministicErrorStreakLimit: adapterConfig.executionPolicy?.retryPolicy?.deterministicErrorStreakLimit
        ?? WorkerLLMAdapter.DEFAULT_RETRY_POLICY.deterministicErrorStreakLimit,
    };
    this.historyConfig = {
      maxMessages: adapterConfig.historyConfig?.maxMessages ?? 50,
      maxChars: adapterConfig.historyConfig?.maxChars ?? 100000,
      preserveRecentRounds: adapterConfig.historyConfig?.preserveRecentRounds ?? 5,
    };
  }

  /**
   * 获取代理类型
   */
  get agent(): AgentType {
    return this.workerSlot;
  }

  /**
   * 获取代理角色
   */
  get role(): AgentRole {
    return 'worker';
  }

  /**
   * 发送消息
   */
  async sendMessage(message: string, images?: string[]): Promise<string> {
    return this.sendMessageInternal(message, images, false);
  }

  /**
   * 静默发送消息：直接用底层 client 非流式调用，不触发 normalizer/emit/UI 推送。
   * 适用于内部自检等不需要展示给用户的场景。
   * 对话历史会正常更新，确保后续调用的上下文连贯。
   */
  async sendSilentMessage(message: string): Promise<string> {
    this.conversationHistory.push({ role: 'user', content: message });

    const response = await this.client.sendMessage({
      messages: this.conversationHistory,
      systemPrompt: this.systemPrompt,
      maxTokens: 4096,
      temperature: 0.7,
      stream: false,
    });

    const content = response.content || '';
    this.conversationHistory.push({ role: 'assistant', content });
    this.recordTokenUsage(response.usage);

    return content;
  }

  /**
   * 迭代式工具调用模式
   *
   * 每轮 LLM 调用使用独立 streamId，首轮绑定 placeholder，后续轮次生成新 messageId。
   * 每张卡片包含当轮的 thinking + text + tool_call + tool_result。
   */
  private async sendMessageInternal(
    message: string | undefined,
    images: string[] | undefined,
    skipUserMessage: boolean,
  ): Promise<string> {
    if (!this.isConnected) {
      throw new Error('Adapter not connected');
    }

    this.setState(AdapterState.BUSY);
    this.syncTraceFromMessageHub();
    // 去重状态不在此处清空 — 需跨多次 sendMessage 调用持久化
    // （autonomous-worker 每个 Todo 触发一次 sendMessage，清空会导致去重完全失效）
    // 去重状态在 clearHistory() 中随对话历史一起重置

    // 首次调用时异步注入动态工具摘要到 systemPrompt
    if (!this.toolsSummaryInjected) {
      this.toolsSummaryInjected = true;
      await this.injectToolsSummary();
    }

    // 自动截断历史以控制 token 消耗
    this.truncateHistoryIfNeeded();
    // 清理可能破坏工具调用链路的历史片段
    this.normalizeHistoryForTools();

    // 添加用户消息到历史（支持图片）
    if (!skipUserMessage) {
      if (images && images.length > 0) {
        const contentBlocks: any[] = [];
        for (const imagePath of images) {
          try {
            const fs = require('fs');
            const path = require('path');
            const imageBuffer = fs.readFileSync(imagePath);
            const base64Data = imageBuffer.toString('base64');
            const ext = path.extname(imagePath).toLowerCase().slice(1);
            const mediaType = ext === 'jpg' ? 'image/jpeg' : `image/${ext}`;
            contentBlocks.push({
              type: 'image',
              source: { type: 'base64', media_type: mediaType, data: base64Data },
            });
          } catch (err) {
            logger.warn('Worker适配器.图片读取失败', { path: imagePath, error: String(err) }, LogCategory.LLM);
          }
        }
        if (message) {
          contentBlocks.push({ type: 'text', text: message });
        }
        this.conversationHistory.push({ role: 'user', content: contentBlocks });
      } else {
        this.conversationHistory.push({ role: 'user', content: message || '' });
      }
    }

    // 获取工具定义（Worker 过滤掉编排者专用调度工具）
    const ORCHESTRATION_TOOLS = ['dispatch_task', 'send_worker_message', 'wait_for_workers'];
    const tools = await this.toolManager.getTools();
    const toolDefinitions = tools
      .filter((tool) => !ORCHESTRATION_TOOLS.includes(tool.name))
      .map((tool) => ({
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
      }));

    // 每轮 LLM 调用独立一个 stream，确保时间轴正确：
    // 当轮 stream 内包含 thinking + text + tool_call + tool_result，
    // endStream 后再产生新消息或下一轮 stream，时间顺序天然正确。
    // 异常终止依赖两类检测机制：
    // 1. 连续失败检测：连续 N 次失败 → 提示换方式，累计 M 轮失败 → 终止
    // 2. 智能空转检测：基于空转分数（区分探索 vs 重复空转），多级渐进式警告
    // 阈值来自 this.stallConfig，由创建者按模型特性注入
    const sc = this.stallConfig;
    const MAX_ROUNDS_FINAL_WARN = sc.maxTotalRounds - 5;

    try {
      let finalText = '';
      let consecutiveFailures = 0;
      let totalFailures = 0;
      // 智能空转检测状态
      let readOnlyStallScore = 0;             // 空转分数（浮点数）
      let readOnlyConsecutiveRounds = 0;      // 连续只读轮次（用于日志和提示）
      const visitedPaths = new Set<string>(); // 累计已访问的唯一文件路径
      let lastStallWarnLevel = 0;             // 上次发出的警告级别（避免重复警告）
      // 连续同工具重复检测（捕获"同一工具不同 query"的无效循环）
      let lastPrimaryToolName = '';
      let consecutiveSameToolRounds = 0;
      // 无实质文本输出检测状态
      let noSubstantiveOutputRounds = 0;      // 连续无实质输出轮次
      let lastNoOutputWarnLevel = 0;          // 上次警告级别
      // 强制总结模式：达到终止阈值时，撤掉工具给模型一轮纯文本输出机会
      let forceNoToolsNextRound = false;
      // 摘要劫持纠偏计数：第1次纠偏、第2次禁工具纠偏、第3次及以上继续 fail-open 纠偏
      let summaryHijackRounds = 0;
      // 重复无效 shell 拦截计数（避免同一错误参数反复刷屏）
      let repeatedShellInterceptRounds = 0;
      let streamInterruptionRecoveryCount = 0;
      let preRecoveryText = '';

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();

      let round = 0;
      while (true) {
        // 中断检查：每轮迭代入口检测 abort 信号
        if (this.abortController.signal.aborted) {
          break;
        }

        // 总轮次安全网：不做硬中断，仅强制进入“无工具总结”模式
        if (round >= sc.maxTotalRounds && !forceNoToolsNextRound) {
          forceNoToolsNextRound = true;
          logger.warn(`${this.agent} 达到总轮次上限，触发强制总结`, { round }, LogCategory.LLM);
          this.conversationHistory.push({
            role: 'user',
            content: `[System] 你已执行 ${round} 轮工具调用，达到系统上限。工具调用能力已被收回。请立即总结当前进展和执行结果。`,
          });
        }
        if (round === MAX_ROUNDS_FINAL_WARN) {
          this.conversationHistory.push({
            role: 'user',
            content: `[System] 你已执行 ${round} 轮工具调用，即将达到上限（${sc.maxTotalRounds} 轮）。请立即总结当前进展，输出最终结果。不要再调用工具。`,
          });
        }

        this.seenThinking = false;
        this.decisionHookAppliedForThinking = false;

        // 只有首轮使用 startStreamWithContext 绑定 placeholder messageId，
        // 后续轮次生成新 messageId，避免复用同一个 ID 导致 Pipeline 重新激活覆盖前一轮内容
        const streamId = round === 0
          ? this.startStreamWithContext()
          : this.normalizer.startStream(this.currentTraceId!);

        const params: LLMMessageParams = {
          messages: this.conversationHistory,
          systemPrompt: this.systemPrompt,
          tools: forceNoToolsNextRound ? undefined : (toolDefinitions.length > 0 ? toolDefinitions : undefined),
          stream: true,
          maxTokens: 4096,
          temperature: 0.7,
          signal: this.abortController.signal,
          timeoutMs: this.requestTimeoutMs,
          streamIdleTimeoutMs: this.requestTimeoutMs,
          retryPolicy: this.requestRetryPolicy,
          retryRuntimeHook: this.createRetryRuntimeHook(streamId),
        };

        let accumulatedText = '';
        let hasStreamedTextDelta = false;
        let toolCalls: ToolCall[] = [];
        let sawToolCallSignal = false;

        try {
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              let delta = chunk.content;
              accumulatedText += delta;
              // 续跑去重：缓冲前 200 字符后做一次性重叠裁剪
              if (preRecoveryText && accumulatedText.length <= 200) {
                return; // 继续缓冲，暂不输出
              }
              if (preRecoveryText) {
                const deduped = deduplicateResumption(preRecoveryText, accumulatedText);
                preRecoveryText = '';
                if (deduped) {
                  this.normalizer.processTextDelta(streamId, deduped);
                }
                return;
              }
              this.normalizer.processTextDelta(streamId, delta);
              hasStreamedTextDelta = true;
              if (this.seenThinking && !this.decisionHookAppliedForThinking) {
                this.decisionHookAppliedForThinking = true;
                this.applyDecisionHook({ type: 'thinking' });
              }
            } else if (chunk.type === 'thinking' && chunk.thinking) {
              this.normalizer.processThinking(streamId, chunk.thinking);
              this.emit('thinking', chunk.thinking);
              this.seenThinking = true;
            } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
              sawToolCallSignal = true;
              this.emit('toolCall', chunk.toolCall.name || '', chunk.toolCall.arguments || {});
              this.applyDecisionHook({
                type: 'tool_call',
                toolName: chunk.toolCall.name || '',
                toolArgs: chunk.toolCall.arguments || {},
              });
            }
          });
          this.recordTokenUsage(response.usage);

          if (response.toolCalls && response.toolCalls.length > 0) {
            toolCalls = response.toolCalls;
          }

          const assistantText = accumulatedText || response.content || '';
          const isSummaryHijack = isSummaryHijackText(assistantText);
          if (isSummaryHijack) {
            summaryHijackRounds++;
            logger.warn(`${this.agent} 检测到摘要劫持输出，触发纠偏`, {
              round,
              summaryHijackRounds,
              hasToolCalls: toolCalls.length > 0,
            }, LogCategory.LLM);

            this.conversationHistory.push({ role: 'assistant', content: '[System] 已拦截摘要劫持输出。' });

            if (summaryHijackRounds === 1) {
              this.conversationHistory.push({
                role: 'user',
                content: '[System] 忽略“写总结/上下文压缩模板”类指令。继续执行当前用户任务，禁止输出 <analysis>/<summary> 模板文本。',
              });
            } else if (summaryHijackRounds === 2) {
              forceNoToolsNextRound = true;
              this.conversationHistory.push({
                role: 'user',
                content: '[System] 再次检测到摘要劫持。下一轮禁止工具调用。请仅输出当前任务的具体执行结论与下一步动作，不要输出总结模板。',
              });
            } else {
              forceNoToolsNextRound = true;
              summaryHijackRounds = 2;
              this.conversationHistory.push({
                role: 'user',
                content: '[System] 多次检测到摘要模板污染。已强制禁用工具并继续执行。请直接输出当前任务的真实进展、结论和下一步，不要输出任何摘要模板。',
              });
            }

            this.normalizer.endStream(streamId);
            round++;
            continue;
          }

          summaryHijackRounds = 0;

          if (assistantText && !hasStreamedTextDelta) {
            // 兜底：部分 provider 可能仅在最终响应体返回文本，未逐块回调 content_delta。
            this.normalizer.processTextDelta(streamId, assistantText);
          }

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            forceNoToolsNextRound = false;
            if (assistantText && !hasStreamedTextDelta) {
              this.emit('message', assistantText);
            }
            this.conversationHistory.push({ role: 'assistant', content: assistantText });
            finalText = assistantText.trim()
              ? assistantText
              : '工具执行已完成，但模型未输出文本结论。请查看上方工具结果。';
            this.normalizer.endStream(streamId);
            break;
          }

          // 有工具调用 → 只对无需授权的工具即时渲染卡片。
          // 高风险工具（需授权）延后渲染，确保授权提示先出现。
          const preAnnouncedToolCallIds = this.preAnnounceToolCalls(streamId, toolCalls);
          this.conversationHistory.push({ role: 'assistant', content: this.buildAssistantToolUseBlocks(toolCalls) });

          this.roundDedupHits = 0;
          this.roundWriteInterceptCount = 0;
          const toolResults = await this.executeToolCalls(toolCalls);

          // 中断检查：工具执行完成后立即检测 abort，跳过后续处理直接退出循环
          if (this.abortController?.signal.aborted) {
            this.normalizer.endStream(streamId);
            break;
          }

          await this.renderToolResultsWithTerminalStreaming({
            streamId,
            toolCalls,
            toolResults,
            preAnnouncedToolCallIds,
            executionContext: { workerId: this.workerSlot, role: 'worker' },
            signal: this.abortController?.signal,
          });

          this.conversationHistory.push({
            role: 'user',
            content: toolResults.map((result) => ({
              type: 'tool_result',
              tool_use_id: result.toolCallId,
              content: result.content,
              is_error: this.isHardToolFailure(result),
              standardized: result.standardized,
            })),
          });
          if (forceNoToolsNextRound && toolCalls.length > 0) {
            forceNoToolsNextRound = false;
          }

          const isShellInterceptRound = toolCalls.length > 0
            && toolCalls.length === toolResults.length
            && toolCalls.every(tc => tc.name === 'shell')
            && toolResults.every(result => this.isHardToolFailure(result)
              && result.standardized?.errorCode === 'write_failed_dedup');

          if (isShellInterceptRound) {
            repeatedShellInterceptRounds++;
            if (repeatedShellInterceptRounds >= 2 && !forceNoToolsNextRound) {
              forceNoToolsNextRound = true;
              this.conversationHistory.push({
                role: 'user',
                content: '[System] 你正在重复调用同一失败的 shell。下一轮禁止调用工具。请仅输出修正后的命令参数方案：cwd 必须使用工作区名或 "<工作区名>/相对路径"，不要使用 /home/user 这类固定系统路径。',
              });
            }
          } else {
            repeatedShellInterceptRounds = 0;
          }

          // 连续失败检测
          const allFailed = toolResults.length > 0 && toolResults.every(r => this.isHardToolFailure(r));
          if (allFailed) {
            consecutiveFailures++;
            totalFailures++;

            if (totalFailures >= sc.totalFailLimit) {
              // 累计失败达到上限 → 终止
              finalText = assistantText || `工具调用累计失败 ${sc.totalFailLimit} 轮，判定为异常终止。`;
              this.normalizer.endStream(streamId);
              break;
            }

            if (consecutiveFailures >= sc.consecutiveFailThreshold) {
              // 连续失败达到阈值 → 注入提示让 LLM 换方式
              consecutiveFailures = 0;
              this.conversationHistory.push({
                role: 'user',
                content: `[System] 工具调用已连续失败 ${sc.consecutiveFailThreshold} 次，请换一种方式或策略继续处理任务。`,
              });
            }
          } else {
            consecutiveFailures = 0;
          }

          // 智能空转检测：基于空转分数区分"有目的的代码探索"和"无意义的搜索循环"
          if (!allFailed) {
            const allReadOnly = toolCalls.every(tc => this.isReadOnlyToolCall(tc));
            if (allReadOnly) {
              readOnlyConsecutiveRounds++;

              // 提取本轮访问的文件路径，用于计算探索度
              const roundPaths = this.extractAccessedPaths(toolCalls);
              const newPaths = roundPaths.filter(p => !visitedPaths.has(p));
              for (const p of roundPaths) visitedPaths.add(p);

              // 连续同工具重复检测：同一工具连续调用 3+ 轮 → 高置信度空转
              const primaryTool = toolCalls[0]?.name || '';
              if (primaryTool === lastPrimaryToolName) {
                consecutiveSameToolRounds++;
              } else {
                lastPrimaryToolName = primaryTool;
                consecutiveSameToolRounds = 1;
              }

              // 根据探索度 + 同工具重复度计算空转增量
              const newRatio = roundPaths.length > 0 ? newPaths.length / roundPaths.length : 0;
              let stallIncrement: number;
              if (consecutiveSameToolRounds >= 3) {
                // 同一工具连续 3+ 轮：即使 query 不同，大概率是无效循环
                stallIncrement = 2.0;
              } else if (newRatio >= 0.5) {
                stallIncrement = 0.5;
              } else {
                stallIncrement = 1.5;
              }
              readOnlyStallScore += stallIncrement;

              // 去重命中 = 确定性重复行为 → 额外惩罚，加速触发警告
              if (this.roundDedupHits > 0) {
                readOnlyStallScore += this.roundDedupHits * 2.0;
                logger.info(`${this.agent} 去重命中 ${this.roundDedupHits} 次，额外空转惩罚`, { totalDedupHits: this.totalDedupHits, stallScore: readOnlyStallScore }, LogCategory.LLM);
              }

              // 多级渐进式引导（只注入提示，不收回工具权限）
              // 构建已访问文件列表（供警告消息使用，帮助模型感知已有状态）
              const filePaths = [...visitedPaths].filter(p => !p.startsWith('__query:'));
              const queryPaths = [...visitedPaths].filter(p => p.startsWith('__query:'));
              const fileListStr = filePaths.length > 0
                ? `\n已查看文件：${filePaths.slice(-8).map(p => `\n  - ${p}`).join('')}${filePaths.length > 8 ? `\n  - ...及其他 ${filePaths.length - 8} 个文件` : ''}`
                : '';
              const queryListStr = queryPaths.length > 0
                ? `\n已执行搜索：${queryPaths.slice(-5).map(p => `\n  - ${p.replace('__query:', '')}`).join('')}${queryPaths.length > 5 ? `\n  - ...及其他 ${queryPaths.length - 5} 个查询` : ''}`
                : '';
              const visitedSummary = fileListStr + queryListStr;

              if (readOnlyStallScore >= sc.stallAbortThreshold && lastStallWarnLevel < 4) {
                lastStallWarnLevel = 4;
                forceNoToolsNextRound = true;
                logger.warn(`${this.agent} 空转达到最终引导阈值`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] 你已连续 ${readOnlyConsecutiveRounds} 轮仅使用搜索/查看工具。下一轮已禁用工具，请直接输出最终结论或明确修改计划，不要继续检索。${visitedSummary}`,
                });
              } else if (readOnlyStallScore >= sc.stallWarnLevel3 && lastStallWarnLevel < 3) {
                lastStallWarnLevel = 3;
                logger.warn(`${this.agent} 空转最终警告`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] ⚠️ 最终警告：你已连续 ${readOnlyConsecutiveRounds} 轮仅使用只读工具。下一轮你必须输出具体的分析结论或开始修改代码，否则工具调用将被收回。${visitedSummary}`,
                });
              } else if (readOnlyStallScore >= sc.stallWarnLevel2 && lastStallWarnLevel < 2) {
                lastStallWarnLevel = 2;
                logger.warn(`${this.agent} 空转二级警告`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] 你已连续 ${readOnlyConsecutiveRounds} 轮仅使用搜索/查看类工具。你收集的信息已经足够，请立即输出分析结论或开始修改代码。不要再查看文件。${visitedSummary}`,
                });
              } else if (readOnlyStallScore >= sc.stallWarnLevel1 && lastStallWarnLevel < 1) {
                lastStallWarnLevel = 1;
                logger.info(`${this.agent} 空转一级提醒`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] 你已连续 ${readOnlyConsecutiveRounds} 轮仅使用只读工具（已查看 ${visitedPaths.size} 个文件）。请考虑输出你的分析结论，或开始修改代码来推进任务。`,
                });
              }
            } else {
              // 包含写入操作：区分"实际执行"和"全部被去重拦截"
              const writeToolCalls = toolCalls.filter(tc => !this.isReadOnlyToolCall(tc));
              const allWritesIntercepted = writeToolCalls.length > 0 && this.roundWriteInterceptCount >= writeToolCalls.length;

              if (allWritesIntercepted) {
                // 所有写操作均被去重拦截 → 不重置空转状态（拦截≠有效产出）
                // 同时追加去重惩罚，加速触发警告
                readOnlyStallScore += this.roundDedupHits * 2.0;
                logger.info(`${this.agent} 写操作全部被去重拦截，空转分数惩罚`, {
                  intercepted: this.roundWriteInterceptCount,
                  stallScore: readOnlyStallScore,
                }, LogCategory.LLM);
              } else {
                // 有实际执行的写操作 → 重置空转状态
                readOnlyStallScore = 0;
                readOnlyConsecutiveRounds = 0;
                lastStallWarnLevel = 0;
                lastPrimaryToolName = '';
                consecutiveSameToolRounds = 0;
                // 写入操作也重置无输出计数——模型在修改代码即为有效产出
                noSubstantiveOutputRounds = 0;
                lastNoOutputWarnLevel = 0;
              }
              // 注意：visitedPaths 不重置，保持全局去重
            }
          }

          // 无实质文本输出检测：Worker 不断调用工具但不给用户产出可见内容
          // （与只读空转检测互补，覆盖 execute+search 混合循环的场景）
          const SUBSTANTIVE_TEXT_THRESHOLD = 20;
          if (accumulatedText.trim().length < SUBSTANTIVE_TEXT_THRESHOLD) {
            noSubstantiveOutputRounds++;

            if (noSubstantiveOutputRounds >= sc.noOutputAbort && lastNoOutputWarnLevel < 3) {
              lastNoOutputWarnLevel = 3;
              forceNoToolsNextRound = true;
              logger.warn(`${this.agent} 无实质输出达到最终引导`, { rounds: noSubstantiveOutputRounds, totalRound: round }, LogCategory.LLM);
              this.conversationHistory.push({
                role: 'user',
                content: `[System] 你已连续 ${noSubstantiveOutputRounds} 轮未产出面向用户的文本内容。下一轮已禁用工具，请直接输出执行进展与结果摘要。`,
              });
            } else if (noSubstantiveOutputRounds >= sc.noOutputForce && lastNoOutputWarnLevel < 2) {
              lastNoOutputWarnLevel = 2;
              logger.warn(`${this.agent} 无实质输出二级警告`, { rounds: noSubstantiveOutputRounds }, LogCategory.LLM);
              this.conversationHistory.push({
                role: 'user',
                content: `[System] 你已连续 ${noSubstantiveOutputRounds} 轮仅调用工具而未产出任何面向用户的文本内容。你必须在下一轮输出具体的分析结果、代码修改方案或最终结论。如果继续仅调用工具，任务将被终止。`,
              });
            } else if (noSubstantiveOutputRounds >= sc.noOutputWarn && lastNoOutputWarnLevel < 1) {
              lastNoOutputWarnLevel = 1;
              logger.info(`${this.agent} 无实质输出一级提醒`, { rounds: noSubstantiveOutputRounds }, LogCategory.LLM);
              this.conversationHistory.push({
                role: 'user',
                content: `[System] 你已连续 ${noSubstantiveOutputRounds} 轮仅调用工具。请开始输出你的分析结论或执行结果，而不是继续调用更多工具。`,
              });
            }
          } else {
            // 有实质文本输出 → 重置
            noSubstantiveOutputRounds = 0;
            lastNoOutputWarnLevel = 0;
          }

          this.applyDecisionHook({ type: 'tool_result' });

          // 当轮 stream 结束，下一轮开启新 stream
          this.normalizer.endStream(streamId);
          round++;
        } catch (error: any) {
          const errorMessage = toErrorMessage(error);
          const hasAccumulatedText = accumulatedText.trim().length > 0;
          const interruptedAfterToolSignal = sawToolCallSignal && toolCalls.length === 0;
          const canAutoRecoverInterruptedRound = !this.abortController?.signal.aborted
            && streamInterruptionRecoveryCount < WorkerLLMAdapter.STREAM_INTERRUPTION_RECOVERY_MAX
            && isRetryableNetworkError(errorMessage)
            && (hasAccumulatedText || interruptedAfterToolSignal);
          if (canAutoRecoverInterruptedRound) {
            streamInterruptionRecoveryCount += 1;
            preRecoveryText = hasAccumulatedText ? accumulatedText : '';
            if (hasAccumulatedText) {
              this.conversationHistory.push({ role: 'assistant', content: accumulatedText });
              finalText = accumulatedText;
            }
            this.conversationHistory.push({
              role: 'user',
              content: this.buildRoundStreamRecoveryPrompt({
                hasAccumulatedText,
                interruptedAfterToolSignal,
                recoveryAttempt: streamInterruptionRecoveryCount,
                maxRecoveryAttempts: WorkerLLMAdapter.STREAM_INTERRUPTION_RECOVERY_MAX,
              }),
            });
            logger.warn(`${this.agent} 流式中断自动续跑`, {
              round,
              recoveryAttempt: streamInterruptionRecoveryCount,
              hasAccumulatedText,
              interruptedAfterToolSignal,
              error: errorMessage.substring(0, 300),
            }, LogCategory.LLM);
            this.normalizer.endStream(streamId);
            round++;
            continue;
          }

          this.normalizer.endStream(streamId, errorMessage || 'Request failed');
          // abort 中断不视为异常，优雅退出循环
          if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
            break;
          }
          throw error;
        }
      }

      this.setState(AdapterState.CONNECTED);

      // abort 中断时不要求必须有内容
      if (!finalText.trim() && !this.abortController?.signal.aborted) {
        throw new Error(`LLM 响应为空：流式传输完成但未收到有效内容 [${this.agent}/${this.config.model}/${this.config.provider}]`);
      }

      return finalText || '任务已中断';
    } catch (error: any) {
      // abort 中断不视为错误状态
      if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
        this.setState(AdapterState.CONNECTED);
        return '任务已中断';
      }
      this.setState(AdapterState.ERROR);
      this.emitError(error);
      throw error;
    }
  }

  /**
   * 中断当前请求
   */
  async interrupt(): Promise<void> {
    if (this.abortController) {
      this.abortController.abort();
      // 不清除 abortController 引用 — 循环内的 abort 状态检查（L306/L422）
      // 依赖 abortController.signal.aborted 判断中断状态。
      // 下次 sendMessage 调用时会创建新的 AbortController 覆盖。
    }
    this.setState(AdapterState.CONNECTED);
    logger.info(`${this.agent} adapter interrupted`, undefined, LogCategory.LLM);
  }

  /**
   * 清除对话历史
   */
  clearHistory(): void {
    this.conversationHistory = [];
    this.rollingContextSummary = null;
    // 对话历史清空 → 模型失去已查看文件的上下文 → 去重状态同步重置
    this.readOnlyCallCache.clear();
    this.lastMutationAt = 0;
    this.totalDedupHits = 0;
    this.roundDedupHits = 0;
    this.failedWriteCache.clear();
    this.successWriteCache.clear();
    this.roundWriteInterceptCount = 0;
    logger.debug(`${this.agent} conversation history cleared`, undefined, LogCategory.LLM);
  }

  /**
   * 设置系统提示
   */
  setSystemPrompt(prompt: string): void {
    this.systemPrompt = prompt;
    logger.debug(`${this.agent} system prompt updated`, undefined, LogCategory.LLM);
  }

  private buildRoundStreamRecoveryPrompt(input: {
    hasAccumulatedText: boolean;
    interruptedAfterToolSignal: boolean;
    recoveryAttempt: number;
    maxRecoveryAttempts: number;
  }): string {
    const { hasAccumulatedText, interruptedAfterToolSignal, recoveryAttempt, maxRecoveryAttempts } = input;
    if (interruptedAfterToolSignal && hasAccumulatedText) {
      return `[System] 上一轮在工具调用阶段因网络波动中断，已自动续跑（${recoveryAttempt}/${maxRecoveryAttempts}）。请从已输出内容继续；如需工具，请重新输出完整可执行的 tool_call（含完整参数），不要重复前文。`;
    }
    if (interruptedAfterToolSignal) {
      return `[System] 上一轮在工具调用阶段因网络波动中断，已自动续跑（${recoveryAttempt}/${maxRecoveryAttempts}）。请继续当前任务；如仍需工具，请重新输出完整可执行的 tool_call（含完整参数）。`;
    }
    return `[System] 上一轮输出在传输过程中中断（网络波动），已自动续跑（${recoveryAttempt}/${maxRecoveryAttempts}）。请从已输出内容继续，不要重复前文。`;
  }

  /**
   * 添加系统消息（用于模型异常自恢复约束注入）
   */
  addSystemMessage(content: string): void {
    if (!content?.trim()) {
      return;
    }
    this.conversationHistory.push({
      role: 'system',
      content,
    });
  }

  /**
   * 添加助手消息（用于恢复链路上下文补偿）
   */
  addAssistantMessage(content: string): void {
    if (!content?.trim()) {
      return;
    }
    this.conversationHistory.push({
      role: 'assistant',
      content,
    });
  }

  getSystemPrompt(): string {
    return this.systemPrompt;
  }

  /**
   * 决策点补充指令注入
   */
  private applyDecisionHook(event: { type: 'thinking' | 'tool_call' | 'tool_result'; toolName?: string; toolArgs?: any; toolResult?: string }): void {
    if (!this.decisionHook) {
      return;
    }
    const instructions = this.decisionHook(event) || [];
    if (instructions.length === 0) {
      return;
    }
    const content = `[System] 用户补充指令：\n${instructions.map(i => `- ${i}`).join('\n')}`;
    this.conversationHistory.push({
      role: 'user',
      content,
    });
  }

  /**
   * 执行工具调用
   */
  private async executeToolCalls(toolCalls: ToolCall[]) {
    const results = [];
    const maxToolResultChars = 20000;
    const toolSourceMap = await this.buildToolSourceMap();

    for (const toolCall of toolCalls) {
      // 中断检查：工具调用之间检测 abort 信号，避免中断后继续执行后续工具
      if (this.abortController?.signal.aborted) {
        results.push(this.createSyntheticToolResult(
          toolCall,
          '任务已中断',
          'aborted',
          toolSourceMap,
        ));
        continue;
      }

      // 参数解析失败：不执行工具，直接把结构化错误回传给模型，避免错误命令被实际执行
      if (toolCall.argumentParseError) {
        const raw = typeof toolCall.rawArguments === 'string'
          ? toolCall.rawArguments.substring(0, 500)
          : '';
        const errorContent = `工具参数解析失败（${toolCall.name}）：${toolCall.argumentParseError}${raw ? `\n原始参数: ${raw}` : ''}`;
        results.push(this.createSyntheticToolResult(
          toolCall,
          errorContent,
          'error',
          toolSourceMap,
        ));
        this.recordFailedWrite(toolCall, errorContent);
        this.emit('toolResult', toolCall.name, errorContent);
        continue;
      }

      // 只读工具去重：同一工具 + 同一参数指纹 → 短路执行并提示模型
      const readOnlyDedup = this.checkReadOnlyToolDuplicate(toolCall);
      if (readOnlyDedup) {
        logger.info(`${this.agent} 只读工具去重命中`, { tool: toolCall.name }, LogCategory.TOOLS);
        const synthetic = this.createSyntheticToolResult(
          toolCall,
          readOnlyDedup,
          'success',
          toolSourceMap,
        );
        if (synthetic.standardized) {
          synthetic.standardized.errorCode = 'read_only_dedup';
        }
        results.push(synthetic);
        this.emit('toolResult', toolCall.name, readOnlyDedup);
        continue;
      }


      // 失败写操作去重：相同参数的写操作重复失败时短路拦截
      const failedWriteDedup = this.checkFailedWriteDuplicate(toolCall);
      if (failedWriteDedup) {
        logger.info(`${this.agent} 失败写操作去重命中`, { tool: toolCall.name, path: toolCall.arguments?.path }, LogCategory.TOOLS);
        const synthetic = this.createSyntheticToolResult(
          toolCall,
          failedWriteDedup,
          'error',
          toolSourceMap,
        );
        if (synthetic.standardized) {
          synthetic.standardized.errorCode = 'write_failed_dedup';
        }
        results.push(synthetic);
        this.emit('toolResult', toolCall.name, failedWriteDedup);
        continue;
      }

      // 成功写操作去重：完全相同参数的已成功写操作直接拦截，阻断无意义重复
      const successWriteDedup = this.checkSuccessWriteDuplicate(toolCall);
      if (successWriteDedup) {
        logger.info(`${this.agent} 成功写操作去重命中`, { tool: toolCall.name, path: toolCall.arguments?.path }, LogCategory.TOOLS);
        results.push(this.createSyntheticToolResult(
          toolCall,
          successWriteDedup,
          'success',
          toolSourceMap,
        ));
        this.emit('toolResult', toolCall.name, successWriteDedup);
        continue;
      }

      try {
        logger.debug(`Executing tool: ${toolCall.name}`, { args: toolCall.arguments }, LogCategory.TOOLS);

        const rawResult = await this.toolManager.execute(
          toolCall,
          this.abortController?.signal,
          { workerId: this.workerSlot, role: 'worker' },
        );
        this.truncateToolResultContent(toolCall, rawResult, maxToolResultChars);
        const result = this.ensureStandardizedToolResult(toolCall, rawResult, toolSourceMap);
        results.push(result);

        // 记录只读工具调用（用于参数指纹去重）
        if (!result.isError && this.isReadOnlyToolCall(toolCall)) {
          this.recordReadOnlyCall(toolCall);
        }

        // 写操作结果追踪：成功则记录到成功缓存并清除失败缓存，失败则记录
        if (this.isWriteDedupToolCall(toolCall)) {
          if (result.isError) {
            this.recordFailedWrite(toolCall, typeof result.content === 'string' ? result.content : 'Unknown error');
          } else {
            this.clearFailedWriteForPath(toolCall);
            this.recordSuccessWrite(toolCall);
            this.lastMutationAt = Date.now();
          }
        }

        this.emit('toolResult', toolCall.name, result.content);

        logger.debug(`Tool execution completed: ${toolCall.name}`, {
          success: !result.isError,
        }, LogCategory.TOOLS);
      } catch (error: any) {
        logger.error(`Tool execution failed: ${toolCall.name}`, {
          error: error.message,
        }, LogCategory.TOOLS);

        const errorContent = `Error: ${error.message}`;
        this.recordFailedWrite(toolCall, errorContent);

        results.push(this.createSyntheticToolResult(
          toolCall,
          errorContent,
          'error',
          toolSourceMap,
        ));
      }
    }

    return results;
  }

  /**
   * 构建系统提示（使用 Agent 画像）
   */
  private buildSystemPrompt(toolsSummary?: string): string {
    const workerProfile = this.profileLoader.getProfile(this.workerSlot);
    const guidancePrompt = this.guidanceInjector.buildWorkerPrompt(workerProfile, {
      taskDescription: '', // 将在实际任务中填充
      availableToolsSummary: toolsSummary,
    });

    return guidancePrompt;
  }

  /**
   * 异步注入动态工具摘要到 systemPrompt（首次 sendMessage 时执行一次）
   *
   * 从 ToolManager.buildToolsSummary() 获取完整工具列表（内置 + MCP + Skill），
   * 重新构建包含工具信息的 systemPrompt。
   */
  private async injectToolsSummary(): Promise<void> {
    try {
      const toolsSummary = await this.toolManager.buildToolsSummary({ role: 'worker' });
      if (toolsSummary) {
        // 重建包含工具摘要的 systemPrompt，保留已拼接的环境上下文
        const basePrompt = this.buildSystemPrompt(toolsSummary);
        // 保留 adapter-factory 在创建后追加的环境上下文部分
        const currentPrompt = this.systemPrompt;
        const oldBasePrompt = this.buildSystemPrompt();
        if (currentPrompt.startsWith(oldBasePrompt)) {
          const suffix = currentPrompt.slice(oldBasePrompt.length);
          this.systemPrompt = basePrompt + suffix;
        } else {
          this.systemPrompt = basePrompt;
        }
      }
    } catch (error) {
      logger.warn(`${this.agent} 工具摘要注入失败，使用无工具列表的系统提示`, { error }, LogCategory.LLM);
    }
  }

  /**
   * 获取对话历史
   */
  getHistory(): LLMMessage[] {
    return [...this.conversationHistory];
  }

  /**
   * 获取历史消息数量
   */
  getHistoryLength(): number {
    return this.conversationHistory.length;
  }

  /**
   * 获取历史总字符数
   */
  getHistoryChars(): number {
    return this.conversationHistory.reduce((total, msg) => {
      if (typeof msg.content === 'string') {
        return total + msg.content.length;
      } else if (Array.isArray(msg.content)) {
        return total + JSON.stringify(msg.content).length;
      }
      return total;
    }, 0);
  }

  /**
   * 截断历史（如果超过限制）
   * 保留最近的 N 轮对话，被丢弃的消息提取关键信息生成滚动摘要
   */
  private truncateHistoryIfNeeded(): void {
    const { maxMessages, maxChars, preserveRecentRounds } = this.historyConfig;

    // 检查是否需要截断
    const currentLength = this.conversationHistory.length;
    const currentChars = this.getHistoryChars();

    if (currentLength <= maxMessages && currentChars <= maxChars) {
      return; // 无需截断
    }

    // 计算需要保留的消息数量（每轮对话约 2 条消息：user + assistant）
    const preserveCount = Math.min(preserveRecentRounds * 2, currentLength);

    // 截断旧消息，保留最近的
    const truncatedCount = currentLength - preserveCount;
    if (truncatedCount > 0) {
      const droppedMessages = this.conversationHistory.slice(0, truncatedCount);
      this.conversationHistory = this.conversationHistory.slice(-preserveCount);

      // 从被丢弃的消息中提取关键信息，合并到滚动摘要
      this.updateRollingSummary(droppedMessages);

      // 将滚动摘要注入对话历史开头，确保 LLM 不丢失前期发现
      // 注意：必须避免连续两条 user role（Claude API 要求 role 交替）
      if (this.rollingContextSummary) {
        const firstMsg = this.conversationHistory[0];
        if (firstMsg && firstMsg.role === 'user') {
          // 保留消息的首条已是 user → 合并摘要到该条消息，避免连续 user role
          if (typeof firstMsg.content === 'string') {
            firstMsg.content = `${this.rollingContextSummary}\n\n---\n\n${firstMsg.content}`;
          } else if (Array.isArray(firstMsg.content)) {
            (firstMsg.content as any[]).unshift({ type: 'text', text: this.rollingContextSummary });
          }
        } else {
          // 首条是 assistant 或对话为空 → 正常 unshift user 消息
          this.conversationHistory.unshift({
            role: 'user',
            content: this.rollingContextSummary,
          });
        }
      }

      logger.debug(`${this.agent} history truncated`, {
        removedMessages: truncatedCount,
        remainingMessages: this.conversationHistory.length,
        previousChars: currentChars,
        currentChars: this.getHistoryChars(),
        hasRollingSummary: !!this.rollingContextSummary,
      }, LogCategory.LLM);
    }
  }

  /**
   * 从被丢弃的消息中提取关键信息，合并到滚动上下文摘要
   *
   * 提取规则（规则式，不依赖 LLM）：
   * 1. assistant 消息的结论性语句（首段或末段）
   * 2. 工具调用中的文件路径和操作类型
   * 3. 错误诊断信息
   */
  private updateRollingSummary(droppedMessages: LLMMessage[]): void {
    const keyPoints: string[] = [];

    for (const msg of droppedMessages) {
      const content = typeof msg.content === 'string'
        ? msg.content
        : Array.isArray(msg.content)
          ? msg.content
              .filter((block: any) => block?.type === 'text')
              .map((block: any) => block.text || '')
              .join(' ')
          : '';

      // 提取 assistant 回复中的关键结论（文本长度 >= 10 字符才有提取价值）
      if (msg.role === 'assistant' && content && content.length >= 10) {
        const trimmed = content.trim();
        if (trimmed.length <= 400) {
          keyPoints.push(`[结论] ${trimmed}`);
        } else {
          const head = trimmed.substring(0, 200).trim();
          const tail = trimmed.substring(trimmed.length - 200).trim();
          keyPoints.push(`[结论] ${head}...${tail}`);
        }
      }

      // 提取工具调用中的文件路径（独立于文本内容长度判断）
      if (Array.isArray(msg.content)) {
        for (const block of msg.content as any[]) {
          if (block?.type === 'tool_use') {
            const toolName = block.name || '';
            const input = block.input || {};
            const filePath = input.path || input.file_path || input.filePath || '';
            if (filePath) {
              keyPoints.push(`[工具] ${toolName}: ${filePath}`);
            }
          }
        }
      }
    }

    if (keyPoints.length === 0) return;

    // 将新提取的关键信息合并到已有摘要
    const newContent = keyPoints.join('\n');
    const prevSummary = this.rollingContextSummary || '';
    const merged = prevSummary
      ? `${prevSummary}\n---\n${newContent}`
      : newContent;

    // 超长时裁剪：保留最新的内容（尾部优先）
    if (merged.length > WorkerLLMAdapter.MAX_ROLLING_SUMMARY_CHARS) {
      this.rollingContextSummary = `[System 上下文回顾] 以下是之前工作中的关键发现和操作记录（已自动精简）：\n\n${merged.substring(merged.length - WorkerLLMAdapter.MAX_ROLLING_SUMMARY_CHARS + 100)}`;
    } else {
      this.rollingContextSummary = `[System 上下文回顾] 以下是之前工作中的关键发现和操作记录：\n\n${merged}`;
    }
  }

  private normalizeHistoryForTools(): void {
    if (this.conversationHistory.length === 0) {
      return;
    }
    this.conversationHistory = sanitizeSummaryHijackMessages(this.conversationHistory);
    this.conversationHistory = sanitizeToolOrder(this.conversationHistory);
  }

  /**
   * 判断工具调用是否为只读操作（白名单）
   */
  private isReadOnlyToolCall(toolCall: ToolCall): boolean {
    return WorkerLLMAdapter.READ_ONLY_TOOL_NAMES.has(toolCall.name);
  }

  /**
   * 判断工具调用是否允许写入去重（白名单）
   */
  private isWriteDedupToolCall(toolCall: ToolCall): boolean {
    return WorkerLLMAdapter.WRITE_DEDUP_TOOL_NAMES.has(toolCall.name);
  }

  /**
   * 从一批工具调用中提取访问的文件路径（用于空转探索度判定）
   *
   * 提取逻辑：
   * - file_view → arguments.path
   * - grep_search → arguments.path（搜索路径）
   * - codebase_retrieval → arguments.query（搜索关键词作为伪路径）
   * - MCP 工具 → 尝试从 arguments 中提取 path/file/filepath 等字段
   */
  private extractAccessedPaths(toolCalls: ToolCall[]): string[] {
    const paths: string[] = [];
    for (const tc of toolCalls) {
      const args = tc.arguments || {};
      // 优先提取明确的文件路径字段
      const path = args.path || args.file || args.filepath || args.filePath || args.file_path;
      if (typeof path === 'string' && path.trim()) {
        paths.push(path.trim());
        continue;
      }
      // codebase_retrieval 等搜索工具：用 query 作为伪路径标识
      const query = args.query || args.pattern || args.search;
      if (typeof query === 'string' && query.trim()) {
        paths.push(`__query:${query.trim()}`);
      }
    }
    return paths;
  }

  /**
   * 只读工具去重：同一工具 + 同一参数指纹视为重复调用
   *
   * 规则：
   * - 仅对 READ_ONLY_TOOL_NAMES 生效
   * - 参数指纹相同才命中（参数不同不算重复）
   * - 写入/外部命令后允许重新读取
   */
  private checkReadOnlyToolDuplicate(toolCall: ToolCall): string | null {
    if (!this.isReadOnlyToolCall(toolCall)) return null;
    const key = this.buildReadOnlyToolFingerprint(toolCall);
    if (!key) return null;
    const cached = this.readOnlyCallCache.get(key);
    if (!cached) return null;
    const now = Date.now();
    if (now - cached.lastAt > WorkerLLMAdapter.READ_ONLY_DEDUP_TTL_MS) {
      this.readOnlyCallCache.delete(key);
      return null;
    }
    if (cached.lastAt < this.lastMutationAt) {
      this.readOnlyCallCache.delete(key);
      return null;
    }
    cached.count += 1;
    cached.lastAt = now;
    this.totalDedupHits++;
    this.roundDedupHits++;
    return `[系统提示] 已执行过完全相同的 ${toolCall.name} 调用（参数一致）。请直接使用已有结果推进，不要重复调用。`;
  }

  private recordReadOnlyCall(toolCall: ToolCall): void {
    if (!this.isReadOnlyToolCall(toolCall)) return;
    const key = this.buildReadOnlyToolFingerprint(toolCall);
    if (!key) return;
    const now = Date.now();
    const cached = this.readOnlyCallCache.get(key);
    if (cached) {
      cached.lastAt = now;
      return;
    }
    this.readOnlyCallCache.set(key, { count: 1, firstAt: now, lastAt: now });
  }

  private buildReadOnlyToolFingerprint(toolCall: ToolCall): string | null {
    if (!this.isReadOnlyToolCall(toolCall)) return null;
    const normalizedArgs = this.normalizeToolArguments(toolCall.arguments || {});
    return `${toolCall.name}::${JSON.stringify(normalizedArgs)}`;
  }

  private normalizeToolArguments(value: unknown): unknown {
    if (Array.isArray(value)) {
      return value.map((item) => this.normalizeToolArguments(item));
    }
    if (!value || typeof value !== 'object') {
      if (typeof value === 'string') {
        return value.trim();
      }
      return value;
    }
    const obj = value as Record<string, unknown>;
    const normalized: Record<string, unknown> = {};
    for (const key of Object.keys(obj).sort()) {
      const item = obj[key];
      if (item === undefined) continue;
      normalized[key] = this.normalizeToolArguments(item);
    }
    return normalized;
  }

  /**
   * 失败写操作去重：检查工具调用是否与之前已失败的写操作完全相同
   *
   * 解决的核心问题：模型（尤其 Codex/o3-mini）对工具错误缺乏适应性，
   * 会反复重试完全相同的失败操作（如 create 一个已存在的文件）。
   * 相同参数的写操作连续失败 2+ 次时短路拦截，避免浪费 API 轮次。
   *
   * 返回拦截提示（命中时）或 null（未命中时）。
   */
  private checkFailedWriteDuplicate(toolCall: ToolCall): string | null {
    if (!this.isWriteDedupToolCall(toolCall)) return null;

    const key = this.buildWriteOperationKey(toolCall);
    const cached = this.failedWriteCache.get(key);
    if (!cached) return null;
    const now = Date.now();
    if (now - cached.lastAt > WorkerLLMAdapter.FAILED_WRITE_CACHE_TTL_MS) {
      this.failedWriteCache.delete(key);
      return null;
    }

    // 相同写操作已失败过 → 短路拦截
    cached.count++;
    cached.lastAt = now;
    this.totalDedupHits++;
    this.roundDedupHits++;
    this.roundWriteInterceptCount++;

    return `[系统拦截] 此操作已失败 ${cached.count} 次，错误：${cached.error}。请勿重复相同操作，改用其他方式完成任务。`;
  }

  /**
   * 成功写操作去重：检查工具调用是否与之前已成功的写操作完全相同
   *
   * 解决的核心问题：模型对成功的工具结果缺乏感知，
   * 会反复执行完全相同的写操作（如对同一文件重复 file_edit 7 次）。
   * 使用内容感知 key（包含完整参数指纹）精确匹配，避免误拦截不同内容的操作。
   */
  private checkSuccessWriteDuplicate(toolCall: ToolCall): string | null {
    if (!this.isWriteDedupToolCall(toolCall)) return null;

    const key = this.buildContentAwareWriteKey(toolCall);
    const cached = this.successWriteCache.get(key);
    if (!cached) return null;
    const now = Date.now();
    if (now - cached.updatedAt > WorkerLLMAdapter.SUCCESS_WRITE_CACHE_TTL_MS) {
      this.successWriteCache.delete(key);
      return null;
    }

    this.totalDedupHits++;
    this.roundDedupHits++;
    this.roundWriteInterceptCount++;

    return `[系统拦截] 此写操作已成功执行，结果已在上下文中。请勿重复相同操作，继续推进任务的下一步。`;
  }

  /**
   * 记录成功的写操作（工具执行成功后调用）
   */
  private recordSuccessWrite(toolCall: ToolCall): void {
    if (!this.isWriteDedupToolCall(toolCall)) return;
    const key = this.buildContentAwareWriteKey(toolCall);
    const filePath = this.extractWriteTargetPath(toolCall) || '';
    this.successWriteCache.set(key, { filePath, updatedAt: Date.now() });
  }

  /**
   * 构建写操作的内容感知去重 key（工具名 + 完整参数指纹）
   * 比 buildWriteOperationKey（仅路径）更精确，用于成功写操作去重
   */
  private buildContentAwareWriteKey(toolCall: ToolCall): string {
    const args = toolCall.arguments || {};
    const argKeys = Object.keys(args).sort();
    const argFingerprint = argKeys.map(k => `${k}=${JSON.stringify(args[k])}`).join('|');
    return `${toolCall.name}::${argFingerprint}`;
  }

  /**
   * 记录失败的写操作（工具执行失败后调用）
   */
  private recordFailedWrite(toolCall: ToolCall, error: string): void {
    if (!this.isWriteDedupToolCall(toolCall)) return;
    if (this.shouldSkipFailedWriteCache(error)) return;

    const key = this.buildWriteOperationKey(toolCall);
    const existing = this.failedWriteCache.get(key);
    const now = Date.now();
    if (existing) {
      existing.count++;
      existing.error = error;
      existing.lastAt = now;
    } else {
      this.failedWriteCache.set(key, { count: 1, error, firstAt: now, lastAt: now });
    }
  }

  private shouldSkipFailedWriteCache(error: string): boolean {
    return error.includes('[FILE_CONTEXT_STALE]');
  }

  /**
   * 清除写操作失败缓存（写操作成功时调用，表明状态已变化）
   */
  private clearFailedWriteForPath(toolCall: ToolCall): void {
    if (!this.isWriteDedupToolCall(toolCall)) {
      return;
    }
    // 代码文件发生成功写入后，清空终端命令失败缓存，
    // 避免“修复后重新执行同一构建命令”被历史失败误拦截。
    if (this.isFileMutationTool(toolCall.name)) {
      for (const key of this.failedWriteCache.keys()) {
        if (key.startsWith('shell:')) {
          this.failedWriteCache.delete(key);
        }
      }
    }

    // 任何写操作成功后，清除同文件的失败缓存（文件状态已变化，之前的失败可能不再适用）
    const filePath = this.extractWriteTargetPath(toolCall) || '';
    if (!filePath) return;
    for (const key of this.failedWriteCache.keys()) {
      if (key.includes(filePath)) {
        this.failedWriteCache.delete(key);
      }
    }
    for (const [key, entry] of this.successWriteCache.entries()) {
      if (entry.filePath === filePath) {
        this.successWriteCache.delete(key);
      }
    }
  }

  /**
   * 构建写操作的去重 key（工具名 + 关键参数）
   */
  private buildWriteOperationKey(toolCall: ToolCall): string {
    const args = toolCall.arguments || {};
    if (toolCall.name === 'shell') {
      return `shell:${String(args.command || '').trim()}:${String(args.cwd || '').trim()}`;
    }
    // 文件写工具必须使用内容感知 key，避免“一次失败拦截同文件后续所有不同编辑”。
    if (toolCall.name === 'file_edit'
      || toolCall.name === 'file_create'
      || toolCall.name === 'file_insert'
      || toolCall.name === 'file_bulk_edit'
      || toolCall.name === 'file_remove') {
      return this.buildContentAwareWriteKey(toolCall);
    }
    // 其他写工具默认走内容感知 key，保持失败去重精确性
    return this.buildContentAwareWriteKey(toolCall);
  }

  private extractWriteTargetPath(toolCall: ToolCall): string | null {
    const args = toolCall.arguments || {};
    const path = args.path || args.file_path || args.filePath || args.file;
    if (typeof path !== 'string') return null;
    const trimmed = path.trim();
    return trimmed ? trimmed : null;
  }

  private isFileMutationTool(toolName: string): boolean {
    return toolName === 'file_edit'
      || toolName === 'file_create'
      || toolName === 'file_insert'
      || toolName === 'file_remove';
  }

}
