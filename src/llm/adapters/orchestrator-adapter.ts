/**
 * Orchestrator LLM 适配器
 * 用于编排者代理
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig } from '../../types/agent-types';
import {
  LLMClient,
  LLMMessageParams,
  LLMMessage,
  ToolCall,
  ToolResult,
  isSummaryHijackText,
  sanitizeSummaryHijackMessages,
  sanitizeToolOrder,
} from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { BUILTIN_TOOL_NAMES } from '../../tools/types';
import { MessageHub } from '../../orchestrator/core/message/message-hub';
import type { PlanMode } from '../../orchestrator/plan-ledger';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';
import { t } from '../../i18n';
import { isModelOriginIssue } from '../../errors/model-origin';
import { hasExplicitWorkerDispatchIntent } from '../../orchestrator/core/request-classifier';
import { normalizeNextSteps } from '../../utils/content-parser';
import { isRetryableNetworkError, toErrorMessage, buildStreamRecoveryPrompt, deduplicateResumption } from '../../tools/network-utils';
import {
  type OrchestratorTerminationReason,
  type ProgressVector,
  type TerminationSnapshot,
  type TerminationCandidate,
  evaluateProgress,
  resolveTerminationReason,
} from './orchestrator-termination';
import {
  buildContinuePrompt,
  buildNoTodoToolLoopPrompt,
  buildOutcomeBlockRequestPrompt,
  buildPseudoToolCallRecoveryPrompt,
  buildSummaryHijackCorrection,
  buildTerminalSynthesisPrompt,
  buildThinkingOnlyOrchestrationRecoveryPrompt,
  buildTerminationFallbackText,
  buildWorkerWaitPreconditionRecoveryPrompt,
  decideNoTodoPlainResponseAction,
  decidePendingTerminalSynthesisAction,
  evaluateNoTodoToolLoopEscalation,
  shouldRequestTerminalSynthesisAfterToolRound,
} from './orchestrator-round-policy';
import {
  OrchestratorDecisionEngine,
  type OrchestratorExecutionBudget,
  type OrchestratorGateEvent,
  type OrchestratorGateState,
} from './orchestrator-decision-engine';

/**
 * 历史管理配置
 */
export interface OrchestratorHistoryConfig {
  /** 最大历史消息数量（默认 40） */
  maxMessages?: number;
  /** 最大历史字符数（默认 100000） */
  maxChars?: number;
  /** 保留最近 N 轮对话（默认 6） */
  preserveRecentRounds?: number;
}

/**
 * Orchestrator 适配器配置
 */
export interface OrchestratorAdapterConfig {
  client: LLMClient;
  normalizer: BaseNormalizer;
  toolManager: ToolManager;
  config: LLMConfig;
  messageHub: MessageHub;  // 🔧 统一消息通道：替代 messageBus
  systemPrompt?: string;
  historyConfig?: OrchestratorHistoryConfig;
  /** 深度任务模式（项目级）：提高总轮次预算 */
  deepTask?: boolean;
}

export interface OrchestratorRuntimeState {
  reason: OrchestratorTerminationReason;
  rounds: number;
  snapshot?: TerminationSnapshot;
  shadow?: {
    enabled: boolean;
    reason: Exclude<OrchestratorTerminationReason, 'unknown'>;
    consistent: boolean;
    note?: string;
  };
  decisionTrace?: OrchestratorDecisionTraceEntry[];
  nextSteps?: string[];
}

export interface OrchestratorDecisionTraceEntry {
  round: number;
  phase: 'no_tool' | 'tool' | 'handoff' | 'finalize';
  action: 'continue' | 'continue_with_prompt' | 'terminate' | 'handoff' | 'fallback';
  requiredTotal: number;
  reason?: Exclude<OrchestratorTerminationReason, 'unknown'>;
  candidates?: string[];
  gateState?: {
    noProgressStreak: number;
    budgetBreachStreak: number;
    externalWaitBreachStreak: number;
    consecutiveUpstreamModelErrors: number;
  };
  note?: string;
  timestamp: number;
}

type MissionOutcomeStatus = 'running' | 'completed' | 'failed';

interface MissionOutcomeBlock {
  status?: MissionOutcomeStatus;
  next_steps?: string[];
}

const MISSION_OUTCOME_START = '[[MISSION_OUTCOME]]';
const MISSION_OUTCOME_END = '[[/MISSION_OUTCOME]]';

class MissionOutcomeExtractor {
  private buffer = '';
  private inBlock = false;
  private latestOutcome: MissionOutcomeBlock | undefined;

  private getStartMarkerHoldbackLength(input: string): number {
    const maxHoldback = Math.min(input.length, MISSION_OUTCOME_START.length - 1);
    for (let len = maxHoldback; len > 0; len -= 1) {
      if (MISSION_OUTCOME_START.startsWith(input.slice(-len))) {
        return len;
      }
    }
    return 0;
  }

  consume(chunk: string): { text: string; outcome?: MissionOutcomeBlock } {
    if (!chunk) {
      return { text: '', outcome: this.latestOutcome };
    }
    this.buffer += chunk;
    let output = '';

    while (this.buffer.length > 0) {
      if (!this.inBlock) {
        const startIndex = this.buffer.indexOf(MISSION_OUTCOME_START);
        if (startIndex === -1) {
          const holdback = this.getStartMarkerHoldbackLength(this.buffer);
          const safeLen = Math.max(0, this.buffer.length - holdback);
          output += this.buffer.slice(0, safeLen);
          this.buffer = this.buffer.slice(safeLen);
          break;
        }
        output += this.buffer.slice(0, startIndex);
        this.buffer = this.buffer.slice(startIndex + MISSION_OUTCOME_START.length);
        this.inBlock = true;
        continue;
      }

      const endIndex = this.buffer.indexOf(MISSION_OUTCOME_END);
      if (endIndex === -1) {
        break;
      }
      const rawJson = this.buffer.slice(0, endIndex).trim();
      this.buffer = this.buffer.slice(endIndex + MISSION_OUTCOME_END.length);
      this.inBlock = false;
      const parsed = this.parseOutcome(rawJson);
      if (parsed) {
        this.latestOutcome = parsed;
      }
    }

    return { text: output, outcome: this.latestOutcome };
  }

  finalize(): { text: string; outcome?: MissionOutcomeBlock } {
    const text = this.inBlock ? '' : this.buffer;
    this.buffer = '';
    this.inBlock = false;
    return { text, outcome: this.latestOutcome };
  }

  private parseOutcome(raw: string): MissionOutcomeBlock | undefined {
    if (!raw) {
      return undefined;
    }
    try {
      const data = JSON.parse(raw);
      if (!data || typeof data !== 'object') {
        return undefined;
      }
      const rawStatus = typeof data.status === 'string' ? data.status.toLowerCase() : '';
      const status = rawStatus === 'running' || rawStatus === 'completed' || rawStatus === 'failed'
        ? (rawStatus as MissionOutcomeStatus)
        : undefined;
      const rawSteps = Array.isArray(data.next_steps)
        ? data.next_steps
        : Array.isArray(data.nextSteps)
          ? data.nextSteps
          : undefined;
      const next_steps = rawSteps
        ? rawSteps.filter((item: unknown) => typeof item === 'string')
        : undefined;
      if (!status && (!next_steps || next_steps.length === 0)) {
        return undefined;
      }
      return { status, next_steps };
    } catch {
      return undefined;
    }
  }
}

const MISSION_OUTCOME_PARTIAL_MARKERS = Array.from(new Set(
  [MISSION_OUTCOME_START, MISSION_OUTCOME_END].flatMap((marker) => {
    const fragments: string[] = [];
    for (let len = marker.length - 1; len > 0; len -= 1) {
      fragments.push(marker.slice(0, len));
    }
    return fragments;
  }),
)).sort((left, right) => right.length - left.length);

function sanitizeMissionOutcomeProtocolText(text: string): string {
  if (!text) {
    return '';
  }

  let sanitized = text;
  while (sanitized.includes(MISSION_OUTCOME_START) || sanitized.includes(MISSION_OUTCOME_END)) {
    const extractor = new MissionOutcomeExtractor();
    const extracted = extractor.consume(sanitized);
    const tail = extractor.finalize();
    const next = `${extracted.text}${tail.text}`;
    if (next === sanitized) {
      break;
    }
    sanitized = next;
  }

  sanitized = sanitized
    .replaceAll(MISSION_OUTCOME_START, '')
    .replaceAll(MISSION_OUTCOME_END, '');

  for (const fragment of MISSION_OUTCOME_PARTIAL_MARKERS) {
    if (sanitized.endsWith(fragment)) {
      return sanitized.slice(0, -fragment.length);
    }
  }

  return sanitized;
}

function extractMissionOutcomePayload(text: string): { text: string; outcome?: MissionOutcomeBlock } {
  if (!text) {
    return { text: '' };
  }
  const extractor = new MissionOutcomeExtractor();
  const extracted = extractor.consume(text);
  const tail = extractor.finalize();
  return {
    text: sanitizeMissionOutcomeProtocolText(`${extracted.text}${tail.text}`),
    outcome: tail.outcome || extracted.outcome,
  };
}

interface OrchestratorTodoSummary {
  id: string;
  missionId?: string;
  assignmentId?: string;
  parentId?: string;
  content?: string;
  status: string;
  worker?: string;
  blockedReason?: string;
  approvalStatus?: string;
  dependsOn?: string[];
  required?: boolean;
  effortWeight?: number;
  waiverApproved?: boolean;
  createdAt?: number;
}

interface CriticalPathBaseline {
  version: number;
  nodeIds: Set<string>;
  nodeWeights: Map<string, number>;
  totalWeight: number;
  pathWeight: number;
}

/**
 * Orchestrator LLM 适配器
 */
export class OrchestratorLLMAdapter extends BaseLLMAdapter {
  /** 滚动摘要最大长度（字符） */
  private static readonly MAX_ROLLING_SUMMARY_CHARS = 2000;
  /** 终止治理：无进展窗口 */
  private static readonly STALLED_WINDOW_SIZE = 5;
  /** 终止治理：外部等待超时（毫秒） */
  private static readonly EXTERNAL_WAIT_SLA_MS = 180_000;
  /** 终止治理：预算门禁要求连续无进展轮次（标准模式） */
  private static readonly BUDGET_NO_PROGRESS_STREAK_THRESHOLD = 2;
  /** 终止治理：预算门禁要求连续无进展轮次（深度模式） */
  private static readonly DEEP_BUDGET_NO_PROGRESS_STREAK_THRESHOLD = 3;
  /** 终止治理：关键路径重基线阈值 */
  private static readonly CP_REBASE_THRESHOLD = 0.10;
  /** 终止治理：上游模型连续错误阈值 */
  private static readonly UPSTREAM_MODEL_ERROR_STREAK = 3;
  /** 终止治理：错误率预算最小样本轮次（避免单轮失败直接误判预算耗尽） */
  private static readonly ERROR_RATE_MIN_SAMPLES = 3;
  /** 终止治理：预算门禁去抖阈值（连续命中轮次） */
  private static readonly BUDGET_BREACH_STREAK_THRESHOLD = 2;
  /** 终止治理：外部等待门禁去抖阈值（连续命中轮次） */
  private static readonly EXTERNAL_WAIT_BREACH_STREAK_THRESHOLD = 2;
  /** 终止治理：预算硬阈值放大系数（超过即立即终止，无需去抖） */
  private static readonly BUDGET_HARD_LIMIT_FACTOR = 1.2;
  /** 终止治理：外部等待硬阈值放大系数（超过即立即终止，无需去抖） */
  private static readonly EXTERNAL_WAIT_HARD_LIMIT_FACTOR = 1.5;
  /** 终止治理：标准模式预算 */
  private static readonly STANDARD_BUDGET = {
    maxDurationMs: 420_000,
    maxTokenUsage: 120_000,
    maxErrorRate: 0.7,
    maxRounds: 30,
  };
  /** 终止治理：深度模式预算 */
  private static readonly DEEP_BUDGET = {
    maxDurationMs: 900_000,
    maxTokenUsage: 280_000,
    maxErrorRate: 0.8,
    maxRounds: 80,
  };
  /** 网络韧性：编排者单次请求超时 */
  private static readonly REQUEST_TIMEOUT_MS = 90_000;
  /** 网络韧性：编排者请求重试策略 */
  private static readonly REQUEST_RETRY_POLICY = {
    maxRetries: 6, // 首次 + 5 次重试
    baseDelayMs: 500,
    retryDelaysMs: [10_000, 20_000, 30_000, 40_000, 50_000],
    retryOnTimeout: true,
    retryOnAllErrors: true,
    maxRetryDurationMs: 240_000,
    deterministicErrorStreakLimit: 3,
  } as const;
  /** 流式中断自动续跑预算（按一次 sendMessageWithTools 计） */
  private static readonly STREAM_INTERRUPTION_RECOVERY_MAX = 2;

  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private historyConfig: Required<OrchestratorHistoryConfig>;
  private rollingContextSummary: string | null = null;
  /** 深度任务模式（项目级）：提高总轮次预算 */
  private readonly deepTask: boolean;
  /** 统一终止/门禁决策引擎（按规划模式区分） */
  private readonly standardDecisionEngine: OrchestratorDecisionEngine;
  private readonly deepDecisionEngine: OrchestratorDecisionEngine;

  /**
   * 临时配置（仅对下一次请求生效）
   */
  private tempSystemPrompt?: string;
  private tempIncludeThinking?: boolean;
  private tempEnableToolCalls?: boolean;
  private tempAllowedToolNames?: string[];
  private tempHistoryMode?: 'session' | 'isolated';
  private tempVisibility?: 'user' | 'system' | 'debug';
  private tempPlanningMode?: PlanMode;
  private lastRuntimeState: OrchestratorRuntimeState = {
    reason: 'unknown',
    rounds: 0,
  };

  constructor(adapterConfig: OrchestratorAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageHub  // 🔧 统一消息通道：使用 messageHub
    );
    this.systemPrompt = adapterConfig.systemPrompt ?? '';
    this.deepTask = adapterConfig.deepTask ?? false;
    this.standardDecisionEngine = this.createDecisionEngine(false);
    this.deepDecisionEngine = this.createDecisionEngine(true);
    this.historyConfig = {
      maxMessages: adapterConfig.historyConfig?.maxMessages ?? 40,
      maxChars: adapterConfig.historyConfig?.maxChars ?? 100000,
      preserveRecentRounds: adapterConfig.historyConfig?.preserveRecentRounds ?? 6,
    };
  }

  private createDecisionEngine(deepTask: boolean): OrchestratorDecisionEngine {
    return new OrchestratorDecisionEngine({
      stalledWindowSize: OrchestratorLLMAdapter.STALLED_WINDOW_SIZE,
      externalWaitSlaMs: OrchestratorLLMAdapter.EXTERNAL_WAIT_SLA_MS,
      upstreamModelErrorStreak: OrchestratorLLMAdapter.UPSTREAM_MODEL_ERROR_STREAK,
      errorRateMinSamples: OrchestratorLLMAdapter.ERROR_RATE_MIN_SAMPLES,
      budgetNoProgressStreakThreshold: deepTask
        ? OrchestratorLLMAdapter.DEEP_BUDGET_NO_PROGRESS_STREAK_THRESHOLD
        : OrchestratorLLMAdapter.BUDGET_NO_PROGRESS_STREAK_THRESHOLD,
      budgetBreachStreakThreshold: OrchestratorLLMAdapter.BUDGET_BREACH_STREAK_THRESHOLD,
      externalWaitBreachStreakThreshold: OrchestratorLLMAdapter.EXTERNAL_WAIT_BREACH_STREAK_THRESHOLD,
      budgetHardLimitFactor: OrchestratorLLMAdapter.BUDGET_HARD_LIMIT_FACTOR,
      externalWaitHardLimitFactor: OrchestratorLLMAdapter.EXTERNAL_WAIT_HARD_LIMIT_FACTOR,
    });
  }

  private resolvePlanningModeForCurrentRequest(): PlanMode {
    return this.tempPlanningMode ?? 'standard';
  }

  private getDecisionEngineForPlanningMode(planningMode: PlanMode): OrchestratorDecisionEngine {
    return planningMode === 'deep' ? this.deepDecisionEngine : this.standardDecisionEngine;
  }

  /**
   * 获取代理类型
   */
  get agent(): AgentType {
    return 'orchestrator';
  }

  /**
   * 获取代理角色
   */
  get role(): AgentRole {
    return 'orchestrator';
  }

  /**
   * 发送消息
   */
  async sendMessage(message: string, images?: string[]): Promise<string> {
    if (!this.isConnected) {
      throw new Error('Adapter not connected');
    }

    this.setState(AdapterState.BUSY);
    this.syncTraceFromMessageHub();
    let messageId: string | null = null;

    // 获取临时配置（使用后清除）
    const effectiveSystemPrompt = this.tempSystemPrompt ?? this.systemPrompt;
    const includeThinking = this.tempIncludeThinking ?? true;
    const enableToolCalls = this.tempEnableToolCalls ?? false;
    const historyMode = this.tempHistoryMode ?? 'session';
    const silent = this.tempVisibility === 'system';
    const planningMode = this.resolvePlanningModeForCurrentRequest();
    this.tempSystemPrompt = undefined;
    this.tempIncludeThinking = undefined;
    this.tempEnableToolCalls = undefined;
    this.tempAllowedToolNames = undefined;
    this.tempHistoryMode = undefined;
    this.tempVisibility = undefined;
    this.tempPlanningMode = undefined;
    this.lastRuntimeState = {
      reason: 'unknown',
      rounds: 0,
    };

    try {
      // ── 统一历史准备（无论是否启用 tool calls，此处为唯一入口） ──
      const useIsolatedHistory = silent || historyMode === 'isolated';
      let preparedHistory: LLMMessage[];

      if (useIsolatedHistory) {
        // system 可见性 / isolated 历史模式：纯空白上下文，不污染编排会话历史
        preparedHistory = [this.buildUserMessage(message, images)];
      } else {
        // session 模式：使用并追加到共享会话历史
        this.conversationHistory = this.normalizeHistoryForTools(this.conversationHistory);
        this.truncateHistoryIfNeeded();
        this.conversationHistory.push(this.buildUserMessage(message, images));
        preparedHistory = this.conversationHistory;
      }

      if (enableToolCalls) {
        const content = await this.sendMessageWithTools(
          preparedHistory,
          effectiveSystemPrompt,
          silent ? 'system' : undefined,
          includeThinking,
          planningMode,
        );
        this.setState(AdapterState.CONNECTED);
        return content;
      }

      const messagesToSend = preparedHistory;

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();

      // Orchestrator 通常不需要工具，但可以根据需要启用
      const params: LLMMessageParams = {
        messages: messagesToSend,
        systemPrompt: effectiveSystemPrompt,
        stream: true,
        maxTokens: 8192, // Orchestrator 可能需要更多 tokens
        temperature: 0.3, // 更低的温度以获得更确定的规划
        signal: this.abortController.signal,
        timeoutMs: OrchestratorLLMAdapter.REQUEST_TIMEOUT_MS,
        streamIdleTimeoutMs: OrchestratorLLMAdapter.REQUEST_TIMEOUT_MS,
        retryPolicy: OrchestratorLLMAdapter.REQUEST_RETRY_POLICY,
      };
      let finalResponse = '';
      let streamInterruptionRecoveryCount = 0;
      let preRecoveryText = '';
      let round = 0;
      while (true) {
        // visibility: 'system' 时不绑定 placeholder，使用独立 messageId，且标记 visibility 让前端拦截
        const streamId = silent
          ? this.normalizer.startStream(this.syncTraceFromMessageHub(), undefined, undefined, 'system')
          : round === 0
            ? this.startStreamWithContext()
            : this.normalizer.startStream(this.currentTraceId!);
        messageId = streamId;
        params.retryRuntimeHook = silent ? undefined : this.createRetryRuntimeHook(streamId);
        const outcomeExtractor = new MissionOutcomeExtractor();
        let streamedResponse = '';
        let hasStreamedTextDelta = false;

        try {
          // 流式调用 LLM
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              const filtered = outcomeExtractor.consume(chunk.content);
              const delta = filtered.text;
              if (!delta) {
                return;
              }
              streamedResponse += delta;
              if (preRecoveryText && streamedResponse.length <= 200) {
                return;
              }
              if (preRecoveryText) {
                const deduped = deduplicateResumption(preRecoveryText, streamedResponse);
                preRecoveryText = '';
                if (deduped) {
                  this.normalizer.processTextDelta(streamId, deduped);
                  this.emit('message', deduped);
                }
                hasStreamedTextDelta = true;
                return;
              }
              this.normalizer.processTextDelta(streamId, delta);
              this.emit('message', delta);
              hasStreamedTextDelta = true;
            } else if (includeThinking && chunk.type === 'thinking' && chunk.thinking) {
              this.normalizer.processThinking(streamId, chunk.thinking);
              this.emit('thinking', chunk.thinking);
            }
          });
          this.recordTokenUsage(response.usage);
          const flushed = outcomeExtractor.finalize();
          if (flushed.text) {
            const finalDelta = sanitizeMissionOutcomeProtocolText(flushed.text);
            if (finalDelta) {
              streamedResponse += finalDelta;
              if (hasStreamedTextDelta) {
                this.normalizer.processTextDelta(streamId, finalDelta);
                this.emit('message', finalDelta);
              }
            }
          }
          finalResponse = sanitizeMissionOutcomeProtocolText(streamedResponse);
          if (!finalResponse && response.content) {
            finalResponse = extractMissionOutcomePayload(response.content).text;
          }
          if (isSummaryHijackText(finalResponse)) {
            logger.warn('Orchestrator.检测到摘要劫持输出_已降级为不中断', {
              model: this.config.model,
              provider: this.config.provider,
              streamed: streamedResponse.length > 0,
            }, LogCategory.LLM);
            finalResponse = '[System] 检测到异常摘要模板输出，已自动忽略。请继续当前任务。';
          }

          if (finalResponse && !hasStreamedTextDelta) {
            // 兜底：部分 provider 仅在最终响应体返回文本，或续跑裁剪后尚未实际下发可见文本。
            this.normalizer.processTextDelta(streamId, finalResponse);
            this.emit('message', finalResponse);
          }
          this.normalizer.sanitizePendingText(streamId, sanitizeMissionOutcomeProtocolText);
          this.normalizer.endStream(streamId);
          break;
        } catch (error: any) {
          const errorMessage = toErrorMessage(error);
          const canAutoRecoverInterruptedRound = !this.abortController?.signal.aborted
            && streamInterruptionRecoveryCount < OrchestratorLLMAdapter.STREAM_INTERRUPTION_RECOVERY_MAX
            && isRetryableNetworkError(errorMessage)
            && streamedResponse.trim().length > 0;
          if (canAutoRecoverInterruptedRound) {
            streamInterruptionRecoveryCount += 1;
            preRecoveryText = sanitizeMissionOutcomeProtocolText(streamedResponse);
            messagesToSend.push({ role: 'assistant', content: streamedResponse });
            messagesToSend.push({
              role: 'user',
              content: buildStreamRecoveryPrompt(t, streamedResponse, streamInterruptionRecoveryCount, OrchestratorLLMAdapter.STREAM_INTERRUPTION_RECOVERY_MAX),
            });
            logger.warn('Orchestrator.单轮流式中断.自动续跑', {
              recoveryAttempt: streamInterruptionRecoveryCount,
              hasAccumulatedText: streamedResponse.trim().length > 0,
              error: errorMessage.substring(0, 300),
            }, LogCategory.LLM);
            this.normalizer.sanitizePendingText(streamId, sanitizeMissionOutcomeProtocolText);
            this.normalizer.endStream(streamId);
            round++;
            continue;
          }
          this.normalizer.sanitizePendingText(streamId, sanitizeMissionOutcomeProtocolText);
          this.normalizer.endStream(streamId, errorMessage || 'Request failed');
          messageId = null;
          throw error;
        }
      }

      // 用户可见请求才会写入编排历史，内部 system 请求不写入
      if (!silent && historyMode !== 'isolated') {
        this.conversationHistory.push({
          role: 'assistant',
          content: finalResponse,
        });
      }
      this.setState(AdapterState.CONNECTED);
      this.lastRuntimeState = {
        reason: 'completed',
        rounds: 1,
      };

      // 🔧 如果流式传输完成但没有内容，抛出明确错误而非静默返回空
      if (!finalResponse.trim()) {
        throw new Error(`LLM 响应为空：流式传输完成但未收到有效内容 [orchestrator/${this.config.model}/${this.config.provider}]`);
      }

      return finalResponse;
    } catch (error: any) {
      // abort 中断不视为错误
      if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
        if (messageId) {
          this.normalizer.sanitizePendingText(messageId, sanitizeMissionOutcomeProtocolText);
          this.normalizer.interruptStream(messageId);
        }
        this.setState(AdapterState.CONNECTED);
        this.lastRuntimeState = {
          reason: 'interrupted',
          rounds: 0,
        };
        return '任务已中断';
      }
      if (messageId) {
        this.normalizer.sanitizePendingText(messageId, sanitizeMissionOutcomeProtocolText);
        this.normalizer.endStream(messageId, error?.message || 'Request failed');
      }
      this.setState(AdapterState.ERROR);
      this.emitError(error);
      throw error;
    }
  }

  /**
   * 构建用户消息（支持图片）
   */
  private buildUserMessage(message: string, images?: string[]): LLMMessage {
    if (images && images.length > 0) {
      const contentBlocks: any[] = [];

      // 添加图片内容块
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
            source: {
              type: 'base64',
              media_type: mediaType,
              data: base64Data,
            },
          });
        } catch (err) {
          logger.warn('Orchestrator适配器.图片读取失败', { path: imagePath, error: String(err) }, LogCategory.LLM);
        }
      }

      // 添加文本内容块
      if (message) {
        contentBlocks.push({
          type: 'text',
          text: message,
        });
      }

      return {
        role: 'user',
        content: contentBlocks,
      };
    }

    // 纯文本消息
    return {
      role: 'user',
      content: message,
    };
  }

  /**
   * 中断当前请求
   */
  async interrupt(): Promise<void> {
    if (this.abortController) {
      this.abortController.abort();
      // 不清除 abortController 引用 — 循环内的 abort 状态检查（L436/L518）
      // 依赖 abortController.signal.aborted 判断中断状态。
      // 下次 sendMessage 调用时会创建新的 AbortController 覆盖。
    }
    this.setState(AdapterState.CONNECTED);
    logger.info('Orchestrator adapter interrupted', undefined, LogCategory.LLM);
  }

  /**
   * 清除对话历史
   */
  clearHistory(): void {
    this.conversationHistory = [];
    this.rollingContextSummary = null;
    logger.debug('Orchestrator conversation history cleared', undefined, LogCategory.LLM);
  }

  /**
   * 设置系统提示
   */
  setSystemPrompt(prompt: string): void {
    this.systemPrompt = prompt;
    logger.debug('Orchestrator system prompt updated', undefined, LogCategory.LLM);
  }

  getSystemPrompt(): string {
    return this.systemPrompt;
  }

  /**
   * 设置临时系统提示（仅对下一次请求生效）
   */
  setTempSystemPrompt(prompt: string): void {
    this.tempSystemPrompt = prompt;
  }
  /**
   * 设置临时 thinking 可见性（仅对下一次请求生效）
   */
  setTempIncludeThinking(enabled: boolean): void {
    this.tempIncludeThinking = enabled;
  }
  /**
   * 设置临时工具调用开关（仅对下一次请求生效）
   */
  setTempEnableToolCalls(enabled: boolean): void {
    this.tempEnableToolCalls = enabled;
  }
  /**
   * 设置临时允许工具白名单（仅对下一次请求生效）
   */
  setTempAllowedToolNames(toolNames: string[]): void {
    this.tempAllowedToolNames = [...toolNames];
  }
  /**
   * 设置临时历史模式（仅对下一次请求生效）
   */
  setTempHistoryMode(mode: 'session' | 'isolated'): void {
    this.tempHistoryMode = mode;
  }
  /**
   * 设置临时规划模式（仅对下一次请求生效）
   */
  setTempPlanningMode(mode: PlanMode): void {
    this.tempPlanningMode = mode;
  }
  /**
   * 设置临时可见性（仅对下一次请求生效）
   * visibility: 'system' 时，LLM 调用跳过 normalizer，不产生前端消息
   */
  setTempVisibility(visibility: 'user' | 'system' | 'debug'): void {
    this.tempVisibility = visibility;
  }

  /**
   * 获取最近一次运行态
   */
  getLastRuntimeState(): OrchestratorRuntimeState {
    return { ...this.lastRuntimeState };
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
   * 保留最近的 N 轮对话
   */
  private truncateHistoryIfNeeded(): void {
    // Micro-Compact：先对旧轮次的 tool_result 进行语义压缩
    this.compactOldToolResults();

    const { maxMessages, maxChars, preserveRecentRounds } = this.historyConfig;

    // 检查是否需要截断
    const currentLength = this.conversationHistory.length;
    const currentChars = this.getHistoryChars();

    if (currentLength <= maxMessages && currentChars <= maxChars) {
      return; // 无需截断
    }

    // 计算需要保留的消息数量（每轮对话约 2 条消息：user + assistant）
    const preserveCount = Math.min(preserveRecentRounds * 2, currentLength);

    // 钉住 index 0（用户原始请求）：截断从 index 1 开始，保留迭代锚点
    const pinnedCount = 1;
    const truncatedCount = currentLength - preserveCount - pinnedCount;
    if (truncatedCount > 0) {
      const droppedMessages = this.conversationHistory.splice(pinnedCount, truncatedCount);

      this.updateRollingSummary(droppedMessages);

      // rolling summary 注入到 index 1（钉住消息之后、保留消息之前）
      if (this.rollingContextSummary) {
        const bridgeMsg = this.conversationHistory[pinnedCount];
        if (bridgeMsg && bridgeMsg.role === 'user') {
          if (typeof bridgeMsg.content === 'string') {
            bridgeMsg.content = `${this.rollingContextSummary}\n\n---\n\n${bridgeMsg.content}`;
          } else if (Array.isArray(bridgeMsg.content)) {
            (bridgeMsg.content as any[]).unshift({ type: 'text', text: this.rollingContextSummary });
          }
        } else {
          this.conversationHistory.splice(pinnedCount, 0, {
            role: 'user',
            content: this.rollingContextSummary,
          });
        }
      }

      logger.debug('Orchestrator history truncated', {
        removedMessages: truncatedCount,
        remainingMessages: this.conversationHistory.length,
        previousChars: currentChars,
        currentChars: this.getHistoryChars(),
        hasRollingSummary: !!this.rollingContextSummary,
      }, LogCategory.LLM);
    }
  }

  /**
   * Micro-Compact：压缩旧轮次的 tool_result
   *
   * 对超过 preserveRecentRounds 之前的消息，将大型 tool_result
   * （尤其是 worker_wait 返回）折叠为简短占位符。
   * 在截断丢弃之前执行，显著缩减 token 消耗并保留语义指针。
   */
  private compactOldToolResults(): void {
    const { preserveRecentRounds } = this.historyConfig;
    const history = this.conversationHistory;
    if (history.length === 0) {
      return;
    }

    // 计算需保护的消息边界：最近 N 轮（每轮约 2 条消息）不压缩
    const protectedCount = Math.min(preserveRecentRounds * 2, history.length);
    const compactBoundary = history.length - protectedCount;
    if (compactBoundary <= 0) {
      return;
    }

    let compactedCount = 0;
    let savedChars = 0;

    for (let i = 0; i < compactBoundary; i++) {
      const msg = history[i];
      if (msg.role !== 'user' || !Array.isArray(msg.content)) {
        continue;
      }

      const blocks = msg.content as any[];
      let modified = false;

      for (let j = 0; j < blocks.length; j++) {
        const block = blocks[j];
        if (block?.type !== 'tool_result' || typeof block.content !== 'string') {
          continue;
        }

        const content = block.content as string;
        // 仅压缩较大的 tool_result（> 500 字符）
        if (content.length <= 500) {
          continue;
        }

        const compacted = this.compactToolResultContent(content);
        if (compacted && compacted.length < content.length) {
          savedChars += content.length - compacted.length;
          blocks[j] = { ...block, content: compacted };
          modified = true;
        }
      }

      if (modified) {
        compactedCount++;
      }
    }

    if (compactedCount > 0) {
      logger.debug('Orchestrator Micro-Compact 已压缩旧轮次 tool_result', {
        compactedMessages: compactedCount,
        savedChars,
        boundary: compactBoundary,
      }, LogCategory.LLM);
    }
  }

  /**
   * 压缩单个 tool_result 的内容
   * 识别 worker_wait / worker_dispatch 的 JSON 返回并提取关键信息
   */
  private compactToolResultContent(content: string): string | null {
    // 尝试解析为 worker_wait 结果
    try {
      const parsed = JSON.parse(content);

      // worker_wait 结果：包含 results 数组和 wait_status
      if (parsed.results && Array.isArray(parsed.results) && 'wait_status' in parsed) {
        return this.compactWaitForWorkersResult(parsed);
      }

      // worker_dispatch 结果：包含 task_id 和 worker
      if (parsed.task_id && parsed.worker) {
        // worker_dispatch 结果通常较短，但如果有大量上下文也压缩
        if (content.length > 800) {
          return `[已折叠: worker_dispatch 结果] task_id=${parsed.task_id}, worker=${parsed.worker}, status=${parsed.status || 'dispatched'}`;
        }
        return null; // 不需要压缩
      }
    } catch {
      // 非 JSON 内容
    }

    // 非结构化大文本：保留首尾摘要
    if (content.length > 1500) {
      const head = content.substring(0, 300).trim();
      const tail = content.substring(content.length - 200).trim();
      return `${head}\n\n[... ${content.length - 500} chars compacted ...]\n\n${tail}`;
    }

    return null; // 不需要压缩
  }

  /**
   * 压缩 worker_wait 返回结果
   * 保留：任务 ID、Worker、状态、修改文件列表
   * 移除：完整 summary 文本、详细 audit 内容
   */
  private compactWaitForWorkersResult(result: any): string {
    const lines: string[] = ['[已折叠: worker_wait 历史结果，关键信息已提取至 PlanLedger]'];

    lines.push(`wait_status: ${result.wait_status}`);
    if (result.timed_out) {
      lines.push(`timed_out: true`);
    }
    if (result.pending_task_ids?.length > 0) {
      lines.push(`pending: ${result.pending_task_ids.join(', ')}`);
    }

    if (Array.isArray(result.results)) {
      for (const r of result.results) {
        const files = r.modified_files?.length > 0
          ? ` files=[${r.modified_files.slice(0, 5).join(', ')}${r.modified_files.length > 5 ? '...' : ''}]`
          : '';
        const errors = r.errors?.length > 0 ? ` errors=${r.errors.length}` : '';
        lines.push(`- ${r.task_id} (${r.worker}): ${r.status}${files}${errors}`);
      }
    }

    if (result.audit) {
      lines.push(`audit: ${result.audit.level} (normal=${result.audit.summary?.normal || 0}, watch=${result.audit.summary?.watch || 0}, intervention=${result.audit.summary?.intervention || 0})`);
    }

    return lines.join('\n');
  }

  private updateRollingSummary(droppedMessages: LLMMessage[]): void {
    const highlights: string[] = [];

    for (const message of droppedMessages) {
      const text = this.extractMessageText(message);
      if (!text) {
        continue;
      }

      if (message.role === 'user') {
        if (/(不要|不能|禁止|必须|务必|严禁|优先|确认)/.test(text)) {
          highlights.push(`- 用户约束: ${text.substring(0, 140)}`);
        }
        continue;
      }

      if (message.role === 'assistant') {
        if (isSummaryHijackText(text)) {
          continue;
        }
        highlights.push(`- 编排进展: ${text.substring(0, 180)}`);
      }
    }

    if (highlights.length === 0) {
      return;
    }

    const previousLines = (this.rollingContextSummary || '')
      .split('\n')
      .map(line => line.trim())
      .filter(line => line.startsWith('- '));
    const mergedLines = Array.from(new Set([...previousLines, ...highlights]));
    const mergedText = mergedLines.join('\n');
    const cropped = mergedText.length > OrchestratorLLMAdapter.MAX_ROLLING_SUMMARY_CHARS
      ? mergedText.substring(mergedText.length - OrchestratorLLMAdapter.MAX_ROLLING_SUMMARY_CHARS)
      : mergedText;

    this.rollingContextSummary = `[System 上下文回顾] 以下为之前轮次的关键上下文（自动精简）：\n${cropped}`;
  }

  private normalizeHistoryForTools(history: LLMMessage[]): LLMMessage[] {
    if (history.length === 0) {
      return history;
    }
    return sanitizeToolOrder(sanitizeSummaryHijackMessages(history));
  }

  private extractMessageText(message: LLMMessage): string {
    if (typeof message.content === 'string') {
      return message.content.trim().replace(/\s+/g, ' ');
    }

    if (!Array.isArray(message.content)) {
      return '';
    }

    const parts: string[] = [];
    for (const block of message.content as any[]) {
      if (!block || typeof block !== 'object') {
        continue;
      }
      if (block.type === 'text' && typeof block.text === 'string') {
        parts.push(block.text);
      } else if (block.type === 'tool_use' && typeof block.name === 'string') {
        parts.push(`调用工具 ${block.name}`);
      }
    }
    return parts.join(' ').trim().replace(/\s+/g, ' ');
  }

  private extractLatestUserMessage(history: LLMMessage[]): string {
    for (let i = history.length - 1; i >= 0; i -= 1) {
      const candidate = history[i];
      if (!candidate || candidate.role !== 'user') {
        continue;
      }
      const text = this.extractMessageText(candidate);
      if (text) {
        return text;
      }
    }
    return '';
  }

  /**
   * 添加系统消息
   */
  addSystemMessage(content: string): void {
    this.conversationHistory.push({
      role: 'system',
      content,
    });
  }

  /**
   * 添加助手消息（用于注入上下文）
   */
  addAssistantMessage(content: string): void {
    this.conversationHistory.push({
      role: 'assistant',
      content,
    });
  }

  /**
   * 编排者工具调用模式（仅在显式启用时）
   *
   * 使用迭代循环（而非递归）实现工具调用链，
   * 整个循环共享一个 streamId，确保用户只看到一条流式消息。
   */
  private async sendMessageWithTools(
    history: LLMMessage[],
    systemPrompt: string,
    visibility: 'user' | 'system' | 'debug' | undefined,
    includeThinking: boolean,
    planningMode: PlanMode,
  ): Promise<string> {
    this.syncTraceFromMessageHub();
    const isTransientSystemCall = visibility === 'system';
    const effectiveDeepTask = planningMode === 'deep';
    const decisionEngine = this.getDecisionEngineForPlanningMode(planningMode);

    const ORCHESTRATOR_HIDDEN_TOOLS = ['todo_split'];
    const allTools = await this.toolManager.getTools();
    const toolDefinitions = allTools
      .filter(tool => {
        if (ORCHESTRATOR_HIDDEN_TOOLS.includes(tool.name)) return false;
        return true;
      })
      .map(tool => ({
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
      }));

    const budget: OrchestratorExecutionBudget = effectiveDeepTask
      ? OrchestratorLLMAdapter.DEEP_BUDGET
      : OrchestratorLLMAdapter.STANDARD_BUDGET;
    const initiatingUserMessage = this.extractLatestUserMessage(history);

    try {
      let finalText = '';
      let finalTextDelivered = false;
      let lastNonEmptyAssistantText = '';
      let totalToolResultCount = 0;
      let loopRounds = 0;
      let toolFailureRounds = 0;
      let noProgressStreak = 0;
      let noTodoOutcomeMissingStreak = 0;
      let noTodoToolRoundStreak = 0;
      let workerWaitProtocolRecoveryCount = 0;
      let thinkingOnlyOrchestrationRecoveryCount = 0;
      let pseudoToolCallRecoveryCount = 0;
      let repeatedNoTodoToolSignatureStreak = 0;
      let lastNoTodoToolSignature = '';
      let consecutiveUpstreamModelErrors = 0;
      let budgetBreachStreak = 0;
      let externalWaitBreachStreak = 0;
      let streamInterruptionRecoveryCount = 0;
      let preRecoveryTextLoop = '';
      const decisionTrace: OrchestratorDecisionTraceEntry[] = [];
      // 摘要劫持纠偏计数：第1次纠偏、第2次禁工具纠偏、第3次及以上继续 fail-open 纠偏
      let summaryHijackRounds = 0;
      let forceNoToolsNextRound = false;
      let pendingTerminalReason: Exclude<OrchestratorTerminationReason, 'unknown'> | null = null;
      let baseline: CriticalPathBaseline | null = null;
      let latestSnapshot: TerminationSnapshot | undefined;
      let lastSnapshot: TerminationSnapshot | null = null;
      let latestOutcomeSteps: string[] = [];
      let terminationReason: Exclude<OrchestratorTerminationReason, 'unknown'> = 'completed';
      let runtimeShadow: OrchestratorRuntimeState['shadow'];
      const loopStartAt = Date.now();
      const loopStartTokenUsage = this.getTotalTokenUsage();
      const loopStartTokenUsed = (loopStartTokenUsage.inputTokens || 0) + (loopStartTokenUsage.outputTokens || 0);
      let pendingTerminalSynthesisRetry = 0;
      let pendingOutcomeBlockOnly = false;
      let pendingOutcomeBlockText = '';

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();
      const usesPersistentVisibleStream = visibility !== 'system';
      const persistentVisibleStreamId = usesPersistentVisibleStream
        ? this.startStreamWithContext()
        : undefined;
      let persistentVisibleStreamFinished = false;
      const sanitizeVisibleStream = (streamId: string | undefined): void => {
        if (!streamId) {
          return;
        }
        this.normalizer.sanitizePendingText(streamId, sanitizeMissionOutcomeProtocolText);
      };
      const endIntermediateRoundStream = (streamId: string): void => {
        sanitizeVisibleStream(streamId);
        if (!usesPersistentVisibleStream) {
          this.normalizer.endStream(streamId);
        }
      };
      const endVisibleStreamWithError = (streamId: string, errorMessage: string): void => {
        sanitizeVisibleStream(usesPersistentVisibleStream ? persistentVisibleStreamId : streamId);
        if (usesPersistentVisibleStream) {
          if (!persistentVisibleStreamFinished && persistentVisibleStreamId) {
            this.normalizer.endStream(persistentVisibleStreamId, errorMessage);
            persistentVisibleStreamFinished = true;
          }
          return;
        }
        this.normalizer.endStream(streamId, errorMessage);
      };
      const interruptVisibleStream = (streamId: string): void => {
        sanitizeVisibleStream(usesPersistentVisibleStream ? persistentVisibleStreamId : streamId);
        if (usesPersistentVisibleStream) {
          if (!persistentVisibleStreamFinished && persistentVisibleStreamId) {
            this.normalizer.interruptStream(persistentVisibleStreamId);
            persistentVisibleStreamFinished = true;
          }
          return;
        }
        this.normalizer.interruptStream(streamId);
      };
      const finishPersistentVisibleStream = (): void => {
        if (!usesPersistentVisibleStream || persistentVisibleStreamFinished || !persistentVisibleStreamId) {
          return;
        }
        sanitizeVisibleStream(persistentVisibleStreamId);
        this.normalizer.endStream(persistentVisibleStreamId);
        persistentVisibleStreamFinished = true;
      };

      let round = 0;
      while (true) {
        // 中断检查：每轮迭代入口检测 abort 信号
        if (this.abortController.signal.aborted) {
          terminationReason = 'external_abort';
          break;
        }
        loopRounds++;

        // 硬限保护：防止条件收敛失败时的无限循环
        if (loopRounds > budget.maxRounds) {
          terminationReason = 'budget_exceeded';
          logger.warn('Orchestrator.Termination.MaxRounds.硬限触发', {
            loopRounds,
            maxRounds: budget.maxRounds,
          }, LogCategory.LLM);
          break;
        }

        // 长任务 history 裁剪：每轮 LLM 调用前检查并截断，防止 context window 溢出
        this.truncateHistoryIfNeeded();

        const streamId = visibility === 'system'
          ? this.normalizer.startStream(this.currentTraceId!, undefined, undefined, 'system')
          : persistentVisibleStreamId!;

        const suppressVisibleText = pendingOutcomeBlockOnly
          && pendingOutcomeBlockText.trim().length > 0
          && usesPersistentVisibleStream;
        const params: LLMMessageParams = {
          messages: history,
          systemPrompt,
          tools: forceNoToolsNextRound ? undefined : (toolDefinitions.length > 0 ? toolDefinitions : undefined),
          stream: true,
          maxTokens: 8192,
          temperature: 0.3,
          signal: this.abortController.signal,
          timeoutMs: OrchestratorLLMAdapter.REQUEST_TIMEOUT_MS,
          streamIdleTimeoutMs: OrchestratorLLMAdapter.REQUEST_TIMEOUT_MS,
          retryPolicy: OrchestratorLLMAdapter.REQUEST_RETRY_POLICY,
          retryRuntimeHook: visibility === 'system' ? undefined : this.createRetryRuntimeHook(streamId),
        };

        const outcomeExtractor = new MissionOutcomeExtractor();
        let missionOutcome: MissionOutcomeBlock | undefined;
        let accumulatedText = '';
        let hasStreamedTextDelta = false;
        let toolCalls: ToolCall[] = [];
        let sawToolCallSignal = false;

        try {
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              const filtered = outcomeExtractor.consume(chunk.content);
              if (filtered.outcome) {
                missionOutcome = filtered.outcome;
              }
              const delta = filtered.text;
              if (!delta) {
                return;
              }
              accumulatedText += delta;
              if (suppressVisibleText) {
                return;
              }
              // 续跑去重：缓冲前 200 字符后一次性裁剪重叠
              if (preRecoveryTextLoop && accumulatedText.length <= 200) {
                return;
              }
              if (preRecoveryTextLoop) {
                const deduped = deduplicateResumption(preRecoveryTextLoop, accumulatedText);
                preRecoveryTextLoop = '';
                if (deduped) {
                  this.normalizer.processTextDelta(streamId, deduped);
                }
                hasStreamedTextDelta = true;
                return;
              }
              this.normalizer.processTextDelta(streamId, delta);
              hasStreamedTextDelta = true;
            } else if (includeThinking && chunk.type === 'thinking' && chunk.thinking) {
              this.normalizer.processThinking(streamId, chunk.thinking);
              this.emit('thinking', chunk.thinking);
            } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
              sawToolCallSignal = true;
              if (chunk.toolCall.id && chunk.toolCall.name) {
                this.normalizer.addToolCall(streamId, {
                  type: 'tool_call',
                  toolName: chunk.toolCall.name,
                  toolId: chunk.toolCall.id,
                  status: 'running',
                  input: JSON.stringify(chunk.toolCall.arguments || {}, null, 2),
                });
              }
              this.emit('toolCall', chunk.toolCall.name || '', chunk.toolCall.arguments || {});
            }
          });
          this.recordTokenUsage(response.usage);
          const flushed = outcomeExtractor.finalize();
          if (flushed.outcome) {
            missionOutcome = flushed.outcome;
          }
          if (flushed.text) {
            const flushedText = sanitizeMissionOutcomeProtocolText(flushed.text);
            if (flushedText) {
              accumulatedText += flushedText;
              if (hasStreamedTextDelta) {
                this.normalizer.processTextDelta(streamId, flushedText);
              }
            }
          }

          if (response.toolCalls && response.toolCalls.length > 0) {
            toolCalls = response.toolCalls;
          }

          let assistantText = sanitizeMissionOutcomeProtocolText(accumulatedText);
          if (!accumulatedText && response.content) {
            const fallback = extractMissionOutcomePayload(response.content);
            assistantText = fallback.text;
            if (fallback.outcome) {
              missionOutcome = fallback.outcome;
            }
          }
          if (suppressVisibleText && preRecoveryTextLoop) {
            preRecoveryTextLoop = '';
          }
          const normalizedOutcomeSteps = normalizeNextSteps(missionOutcome?.next_steps || []);
          const outcomeStatus = missionOutcome?.status;
          latestOutcomeSteps = normalizedOutcomeSteps;
          const isSummaryHijack = isSummaryHijackText(assistantText);
          if (isSummaryHijack) {
            summaryHijackRounds++;
            logger.warn('orchestrator 检测到摘要劫持输出，触发纠偏', {
              round,
              summaryHijackRounds,
              hasToolCalls: toolCalls.length > 0,
            }, LogCategory.LLM);

            history.push({ role: 'assistant', content: '[System] 已拦截摘要劫持输出。' });
            const correction = buildSummaryHijackCorrection(summaryHijackRounds);
            forceNoToolsNextRound = correction.forceNoToolsNextRound;
            summaryHijackRounds = correction.normalizedRounds;
            history.push({
              role: 'user',
              content: correction.prompt,
            });

            endIntermediateRoundStream(streamId);
            round++;
            continue;
          }

          summaryHijackRounds = 0;
          const assistantTextForNoTool = suppressVisibleText ? pendingOutcomeBlockText : assistantText;
          if (assistantText.trim() && !suppressVisibleText) {
            lastNonEmptyAssistantText = assistantText;
            // 文本一旦进入当轮流式管道（含 fallback 的 processTextDelta），
            // 就应视为“已交付”。否则在工具轮触发终止（如 stalled/budget）时，
            // 循环外 finalText fallback 会把同段文本再次回灌，造成重复输出。
            finalTextDelivered = true;
          }
          if (assistantText && !hasStreamedTextDelta && !suppressVisibleText) {
            // 兜底：部分 provider 可能仅在最终响应体返回文本，未逐块回调 content_delta。
            this.normalizer.processTextDelta(streamId, assistantText);
          }

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            forceNoToolsNextRound = false;
            if (assistantTextForNoTool && !hasStreamedTextDelta && !suppressVisibleText) {
              this.emit('message', assistantTextForNoTool);
            }
            if (assistantTextForNoTool.trim() && !suppressVisibleText) {
              finalTextDelivered = true;
            }
            history.push({ role: 'assistant', content: assistantTextForNoTool });

            const progressState = await this.buildTerminationSnapshot({
              round: loopRounds,
              loopStartAt,
              loopStartTokenUsed,
              toolFailureRounds,
              baseline,
              previousSnapshot: lastSnapshot,
            });
            baseline = progressState.baseline;
            latestSnapshot = progressState.snapshot;
            if (progressState.cpRebased || progressState.progressed) {
              noProgressStreak = 0;
            } else {
              noProgressStreak += 1;
            }
            lastSnapshot = progressState.snapshot;
            ({
              budgetBreachStreak,
              externalWaitBreachStreak,
            } = decisionEngine.updateGateStreaks({
              snapshot: progressState.snapshot,
              budget,
              noProgressStreak,
              current: { budgetBreachStreak, externalWaitBreachStreak },
            }));

            const explicitOrchestrationRequest = this.userExplicitlyRequestsOrchestration(initiatingUserMessage);
            if (
              progressState.snapshot.requiredTotal === 0
              && totalToolResultCount === 0
              && toolDefinitions.length > 0
              && explicitOrchestrationRequest
              && !assistantTextForNoTool.trim()
              && thinkingOnlyOrchestrationRecoveryCount < 1
            ) {
              thinkingOnlyOrchestrationRecoveryCount += 1;
              history.push({ role: 'assistant', content: '[System] 已拦截仅 thinking 的编排空转。' });
              history.push({
                role: 'user',
                content: buildThinkingOnlyOrchestrationRecoveryPrompt(),
              });
              logger.warn('orchestrator 检测到仅 thinking 的编排空转，触发纠偏', {
                round,
                thinkingOnlyOrchestrationRecoveryCount,
              }, LogCategory.LLM);
              endIntermediateRoundStream(streamId);
              round++;
              continue;
            }

            const pseudoToolNarrationDetected = this.detectPseudoOrchestrationToolNarration(assistantTextForNoTool);
            if (
              progressState.snapshot.requiredTotal === 0
              && totalToolResultCount === 0
              && toolDefinitions.length > 0
              && pseudoToolNarrationDetected
              && pseudoToolCallRecoveryCount < 1
            ) {
              pseudoToolCallRecoveryCount += 1;
              history.push({ role: 'assistant', content: '[System] 已拦截正文中的伪工具调用描述。' });
              history.push({
                role: 'user',
                content: buildPseudoToolCallRecoveryPrompt(),
              });
              logger.warn('orchestrator 检测到正文伪工具调用描述，触发纠偏', {
                round,
                pseudoToolCallRecoveryCount,
                mentionCount: this.countOrchestrationToolMentions(assistantTextForNoTool),
              }, LogCategory.LLM);
              endIntermediateRoundStream(streamId);
              round++;
              continue;
            }

            if (pendingTerminalReason) {
              const synthesisDecision = decidePendingTerminalSynthesisAction({
                assistantText,
                hasOutcomeSignal: Boolean(outcomeStatus) || normalizedOutcomeSteps.length > 0,
                retryCount: pendingTerminalSynthesisRetry,
                maxRetryCount: 1,
              });
              if (synthesisDecision.action === 'retry') {
                pendingTerminalSynthesisRetry = synthesisDecision.nextRetryCount;
                history.push({
                  role: 'user',
                  content: buildTerminalSynthesisPrompt(
                    pendingTerminalReason,
                    progressState.snapshot,
                    true,
                  ),
                });
                logger.warn('Orchestrator.Termination.Handoff.收尾轮缺少结构化结论，触发补跑', {
                  reason: pendingTerminalReason,
                  round: loopRounds,
                  retry: pendingTerminalSynthesisRetry,
                  missingText: !assistantText.trim(),
                  missingOutcome: !outcomeStatus && normalizedOutcomeSteps.length === 0,
                }, LogCategory.LLM);
                endIntermediateRoundStream(streamId);
                round++;
                continue;
              }
              logger.info('Orchestrator.Termination.Handoff.收尾轮完成', {
                reason: pendingTerminalReason,
                round: loopRounds,
                requiredTotal: progressState.snapshot.requiredTotal,
              }, LogCategory.LLM);
              terminationReason = pendingTerminalReason;
              pendingTerminalReason = null;
              finalText = assistantText.trim()
                ? assistantText
                : (finalText || lastNonEmptyAssistantText || buildTerminationFallbackText(terminationReason));
              runtimeShadow = this.buildShadowTerminationResult({
                decisionEngine,
                snapshot: progressState.snapshot,
                budget,
                noProgressStreak,
                consecutiveUpstreamModelErrors,
                budgetBreachStreak,
                externalWaitBreachStreak,
                primaryReason: terminationReason,
                assistantText: finalText,
              });
              decisionTrace.push(this.createDecisionTraceEntry({
                round: loopRounds,
                phase: 'handoff',
                action: 'terminate',
                reason: terminationReason,
                requiredTotal: progressState.snapshot.requiredTotal,
                candidates: [terminationReason],
                gateState: this.buildGateState(
                  noProgressStreak,
                  consecutiveUpstreamModelErrors,
                  budgetBreachStreak,
                  externalWaitBreachStreak,
                ),
                note: 'pending_terminal_reason_resolved',
              }));
              endIntermediateRoundStream(streamId);
              break;
            }

            const candidates: TerminationCandidate[] = [];
            if (progressState.snapshot.requiredTotal === 0 && assistantTextForNoTool.trim()) {
              noTodoToolRoundStreak = 0;
              repeatedNoTodoToolSignatureStreak = 0;
              lastNoTodoToolSignature = '';
              const noTodoDecision = decideNoTodoPlainResponseAction({
                assistantText: assistantTextForNoTool,
                totalToolResultCount,
                explicitOrchestrationRequest,
                outcomeStatus,
                normalizedOutcomeStepCount: normalizedOutcomeSteps.length,
                noTodoOutcomeMissingStreak,
              });
              noTodoOutcomeMissingStreak = noTodoDecision.nextMissingOutcomeStreak;

              if (noTodoDecision.action === 'terminate_completed') {
                const label = outcomeStatus === 'failed'
                  ? 'no_required_todos_failed'
                  : normalizedOutcomeSteps.length > 0 || Boolean(outcomeStatus)
                    ? 'no_required_todos'
                    : 'plain_response_no_required_todos';
                candidates.push(this.createTerminationCandidate('completed', label));
              } else if (noTodoDecision.action === 'terminate_failed') {
                const label = normalizedOutcomeSteps.length > 0 || Boolean(outcomeStatus)
                  ? 'no_required_todos_failed'
                  : 'no_outcome_block';
                candidates.push(this.createTerminationCandidate('failed', label));
              } else if (noTodoDecision.action === 'continue_with_prompt') {
                pendingOutcomeBlockOnly = false;
                pendingOutcomeBlockText = '';
                history.push({
                  role: 'user',
                  content: buildContinuePrompt(progressState.snapshot),
                });
                endIntermediateRoundStream(streamId);
                round++;
                continue;
              } else if (noTodoDecision.action === 'request_outcome_block') {
                if (assistantTextForNoTool.trim()) {
                  pendingOutcomeBlockOnly = true;
                  pendingOutcomeBlockText = assistantTextForNoTool;
                }
                history.push({
                  role: 'user',
                  content: buildOutcomeBlockRequestPrompt(),
                });
                endIntermediateRoundStream(streamId);
                round++;
                continue;
              }
            } else if (progressState.snapshot.requiredTotal > 0
              && progressState.snapshot.progressVector.terminalRequiredTodos >= progressState.snapshot.requiredTotal
              && progressState.snapshot.runningOrPendingRequired === 0) {
              noTodoOutcomeMissingStreak = 0;
              if (progressState.snapshot.failedRequired > 0) {
                candidates.push(this.createTerminationCandidate('failed', 'required_todos_failed'));
              } else {
                candidates.push(this.createTerminationCandidate('completed', 'required_todos_resolved'));
              }
            }

            const gateState = this.buildGateState(
              noProgressStreak,
              consecutiveUpstreamModelErrors,
              budgetBreachStreak,
              externalWaitBreachStreak,
            );
            const gateEvaluation = decisionEngine.collectBudgetCandidates({
              snapshot: progressState.snapshot,
              budget,
              gateState,
              createCandidate: (reason, label) => this.createTerminationCandidate(reason, label),
            });
            this.logGateEvents(gateEvaluation.events);
            candidates.push(...gateEvaluation.candidates);
            decisionTrace.push(this.createDecisionTraceEntry({
              round: loopRounds,
              phase: 'no_tool',
              action: candidates.length > 0 ? 'terminate' : 'continue',
              reason: candidates.length > 0 ? resolveTerminationReason(candidates).reason : undefined,
              requiredTotal: progressState.snapshot.requiredTotal,
              candidates: candidates.map((item) => item.reason),
              gateState,
            }));

            if (candidates.length > 0) {
              const resolved = resolveTerminationReason(candidates);
              terminationReason = resolved.reason;
              progressState.snapshot.sourceEventIds = resolved.evidenceIds;
              runtimeShadow = this.buildShadowTerminationResult({
                decisionEngine,
                snapshot: progressState.snapshot,
                budget,
                noProgressStreak,
                consecutiveUpstreamModelErrors,
                budgetBreachStreak,
                externalWaitBreachStreak,
                primaryReason: terminationReason,
                assistantText: assistantTextForNoTool,
              });
              finalText = assistantTextForNoTool.trim() ? assistantTextForNoTool : (finalText || lastNonEmptyAssistantText);
              endIntermediateRoundStream(streamId);
              break;
            }

            history.push({
              role: 'user',
              content: buildContinuePrompt(progressState.snapshot),
            });
            endIntermediateRoundStream(streamId);
            round++;
            continue;
          }

          if (pendingOutcomeBlockOnly && toolCalls.length > 0) {
            pendingOutcomeBlockOnly = false;
            pendingOutcomeBlockText = '';
          }

          // 有工具调用 → 只对无需授权的工具即时渲染卡片
          // 需要授权的高风险工具延后到授权完成后再渲染，避免“先出现 edit 卡片后弹授权”。
          const preAnnouncedToolCallIds = this.preAnnounceToolCalls(streamId, toolCalls);
          history.push({ role: 'assistant', content: this.buildAssistantToolUseBlocks(toolCalls) });

          const toolResults = await this.executeToolCalls(toolCalls);
          totalToolResultCount += toolResults.length;

          // 中断检查：工具执行完成后立即检测 abort，跳过后续处理直接退出循环
          if (this.abortController?.signal.aborted) {
            endIntermediateRoundStream(streamId);
            terminationReason = 'external_abort';
            break;
          }

          await this.renderToolResultsWithTerminalStreaming({
            streamId,
            toolCalls,
            toolResults,
            preAnnouncedToolCallIds,
            executionContext: { workerId: 'orchestrator', role: 'orchestrator' },
            signal: this.abortController?.signal,
          });

          history.push({
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
          const allFailed = toolResults.length > 0 && toolResults.every(r => this.isHardToolFailure(r));
          if (allFailed) {
            toolFailureRounds += 1;
          }
          const hasUpstreamModelError = toolResults.some(result =>
            this.isHardToolFailure(result) && isModelOriginIssue(result.content || '')
          );
          consecutiveUpstreamModelErrors = hasUpstreamModelError
            ? consecutiveUpstreamModelErrors + 1
            : 0;

          const progressState = await this.buildTerminationSnapshot({
            round: loopRounds,
            loopStartAt,
            loopStartTokenUsed,
            toolFailureRounds,
            baseline,
            previousSnapshot: lastSnapshot,
          });
          baseline = progressState.baseline;
          latestSnapshot = progressState.snapshot;
          if (progressState.cpRebased || progressState.progressed) {
            noProgressStreak = 0;
          } else {
            noProgressStreak += 1;
          }
          noTodoOutcomeMissingStreak = 0;
          lastSnapshot = progressState.snapshot;
          ({
            budgetBreachStreak,
            externalWaitBreachStreak,
          } = decisionEngine.updateGateStreaks({
            snapshot: progressState.snapshot,
            budget,
            noProgressStreak,
            current: { budgetBreachStreak, externalWaitBreachStreak },
          }));

          if (progressState.snapshot.requiredTotal === 0) {
            noTodoToolRoundStreak += 1;
            const roundSignature = this.buildToolRoundSignature(toolCalls);
            const toolLoopEscalation = evaluateNoTodoToolLoopEscalation({
              roundSignature,
              lastSignature: lastNoTodoToolSignature,
              noTodoToolRoundStreak,
              repeatedSignatureStreak: repeatedNoTodoToolSignatureStreak,
              forceNoToolsNextRound,
            });
            repeatedNoTodoToolSignatureStreak = toolLoopEscalation.repeatedSignatureStreak;
            lastNoTodoToolSignature = toolLoopEscalation.lastSignature;
            forceNoToolsNextRound = toolLoopEscalation.forceNoToolsNextRound;

            if (toolLoopEscalation.shouldEscalate) {
              history.push({
                role: 'user',
                content: buildNoTodoToolLoopPrompt(noTodoToolRoundStreak, repeatedNoTodoToolSignatureStreak),
              });
              endIntermediateRoundStream(streamId);
              round++;
              continue;
            }
          } else {
            noTodoToolRoundStreak = 0;
            repeatedNoTodoToolSignatureStreak = 0;
            lastNoTodoToolSignature = '';
          }

          const candidates: TerminationCandidate[] = [];
          if (progressState.snapshot.requiredTotal > 0
            && progressState.snapshot.progressVector.terminalRequiredTodos >= progressState.snapshot.requiredTotal
            && progressState.snapshot.runningOrPendingRequired === 0) {
            if (progressState.snapshot.failedRequired > 0) {
              candidates.push(this.createTerminationCandidate('failed', 'required_todos_failed'));
            } else {
              candidates.push(this.createTerminationCandidate('completed', 'required_todos_resolved'));
            }
          }
          const gateState = this.buildGateState(
            noProgressStreak,
            consecutiveUpstreamModelErrors,
            budgetBreachStreak,
            externalWaitBreachStreak,
          );
          const gateEvaluation = decisionEngine.collectBudgetCandidates({
            snapshot: progressState.snapshot,
            budget,
            gateState,
            createCandidate: (reason, label) => this.createTerminationCandidate(reason, label),
          });
          this.logGateEvents(gateEvaluation.events);
          candidates.push(...gateEvaluation.candidates);
          const protocolViolationRecovery = this.resolveProtocolViolationRecovery({
            toolResults,
            snapshot: progressState.snapshot,
            userMessage: initiatingUserMessage,
            recoveryCount: workerWaitProtocolRecoveryCount,
          });
          if (protocolViolationRecovery) {
            workerWaitProtocolRecoveryCount = protocolViolationRecovery.nextRecoveryCount;
            decisionTrace.push(this.createDecisionTraceEntry({
              round: loopRounds,
              phase: 'tool',
              action: 'continue_with_prompt',
              requiredTotal: progressState.snapshot.requiredTotal,
              gateState,
              note: `protocol_recovery:${protocolViolationRecovery.label}`,
            }));
            history.push({
              role: 'user',
              content: protocolViolationRecovery.prompt,
            });
            logger.warn('Orchestrator.ProtocolViolation.触发纠偏重试', {
              round: loopRounds,
              requiredTotal: progressState.snapshot.requiredTotal,
              label: protocolViolationRecovery.label,
              errorCodes: protocolViolationRecovery.errorCodes,
            }, LogCategory.LLM);
            endIntermediateRoundStream(streamId);
            round++;
            continue;
          }
          const protocolViolationTermination = this.resolveProtocolViolationTermination({
            toolResults,
            snapshot: progressState.snapshot,
          });
          if (protocolViolationTermination) {
            candidates.push(this.createTerminationCandidate('failed', protocolViolationTermination.label));
            logger.warn('Orchestrator.Termination.ProtocolViolation.触发失败', {
              round: loopRounds,
              requiredTotal: progressState.snapshot.requiredTotal,
              label: protocolViolationTermination.label,
              errorCodes: protocolViolationTermination.errorCodes,
            }, LogCategory.LLM);
          }

          if (candidates.length > 0) {
            const resolved = resolveTerminationReason(candidates);
            progressState.snapshot.sourceEventIds = resolved.evidenceIds;
            decisionTrace.push(this.createDecisionTraceEntry({
              round: loopRounds,
              phase: 'tool',
              action: shouldRequestTerminalSynthesisAfterToolRound(resolved.reason, toolCalls.length)
                ? 'handoff'
                : 'terminate',
              reason: resolved.reason,
              requiredTotal: progressState.snapshot.requiredTotal,
              candidates: candidates.map((item) => item.reason),
              gateState,
            }));
            if (shouldRequestTerminalSynthesisAfterToolRound(resolved.reason, toolCalls.length)) {
              pendingTerminalReason = resolved.reason;
              forceNoToolsNextRound = true;
              logger.info('Orchestrator.Termination.Handoff.进入收尾轮', {
                reason: resolved.reason,
                round: loopRounds,
                requiredTotal: progressState.snapshot.requiredTotal,
              }, LogCategory.LLM);
              history.push({
                role: 'user',
                content: buildTerminalSynthesisPrompt(resolved.reason, progressState.snapshot),
              });
              endIntermediateRoundStream(streamId);
              round++;
              continue;
            }
            terminationReason = resolved.reason;
            runtimeShadow = this.buildShadowTerminationResult({
              decisionEngine,
              snapshot: progressState.snapshot,
              budget,
              noProgressStreak,
              consecutiveUpstreamModelErrors,
              budgetBreachStreak,
              externalWaitBreachStreak,
              primaryReason: terminationReason,
              assistantText,
            });
            finalText = assistantText.trim() ? assistantText : (finalText || lastNonEmptyAssistantText);
            endIntermediateRoundStream(streamId);
            break;
          }

          decisionTrace.push(this.createDecisionTraceEntry({
            round: loopRounds,
            phase: 'tool',
            action: 'continue',
            requiredTotal: progressState.snapshot.requiredTotal,
            candidates: [],
            gateState,
          }));

          // 当轮 stream 结束，工具副作用（subTaskCard 等）已自然排在后面
          endIntermediateRoundStream(streamId);
          round++;
        } catch (error: any) {
          const errorMessage = toErrorMessage(error);
          if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
            interruptVisibleStream(streamId);
            terminationReason = 'external_abort';
            break;
          }
          const hasAccumulatedText = accumulatedText.trim().length > 0;
          const interruptedAfterToolSignal = sawToolCallSignal && toolCalls.length === 0;
          const canAutoRecoverInterruptedRound = !this.abortController?.signal.aborted
            && streamInterruptionRecoveryCount < OrchestratorLLMAdapter.STREAM_INTERRUPTION_RECOVERY_MAX
            && isRetryableNetworkError(errorMessage)
            && (hasAccumulatedText || interruptedAfterToolSignal);
          if (canAutoRecoverInterruptedRound) {
            streamInterruptionRecoveryCount += 1;
            preRecoveryTextLoop = accumulatedText;
            if (hasAccumulatedText) {
              history.push({ role: 'assistant', content: accumulatedText });
              lastNonEmptyAssistantText = accumulatedText;
              finalText = accumulatedText;
            }
            history.push({
              role: 'user',
              content: this.buildRoundStreamRecoveryPrompt({
                hasAccumulatedText,
                interruptedAfterToolSignal,
                recoveryAttempt: streamInterruptionRecoveryCount,
                maxRecoveryAttempts: OrchestratorLLMAdapter.STREAM_INTERRUPTION_RECOVERY_MAX,
                accumulatedText,
              }),
            });
            logger.warn('Orchestrator.流式中断.自动续跑', {
              round: loopRounds,
              recoveryAttempt: streamInterruptionRecoveryCount,
              hasAccumulatedText,
              interruptedAfterToolSignal,
              error: errorMessage.substring(0, 300),
            }, LogCategory.LLM);
            endIntermediateRoundStream(streamId);
            round++;
            continue;
          }

          endVisibleStreamWithError(streamId, errorMessage || 'Request failed');
          throw error;
        }
      }
      if (terminationReason === 'external_abort') {
        if (usesPersistentVisibleStream && !persistentVisibleStreamFinished && persistentVisibleStreamId) {
          sanitizeVisibleStream(persistentVisibleStreamId);
          this.normalizer.interruptStream(persistentVisibleStreamId);
          persistentVisibleStreamFinished = true;
        }
      } else {
        finishPersistentVisibleStream();
      }

      // abort 中断时不要求必须有内容
      if (!finalText.trim() && !this.abortController?.signal.aborted) {
        if (lastNonEmptyAssistantText.trim()) {
          finalText = lastNonEmptyAssistantText;
          finalTextDelivered = true;
          logger.warn('orchestrator 最终轮空文本，回退到最近有效输出', {
            loopRounds,
            totalToolResultCount,
          }, LogCategory.LLM);
        } else if (totalToolResultCount > 0) {
          finalText = '工具执行已完成，但模型未返回最终文本总结。请查看上方工具执行结果。';
          logger.warn('orchestrator 无最终文本，使用工具结果降级总结', {
            loopRounds,
            totalToolResultCount,
          }, LogCategory.LLM);
        } else {
          throw new Error(`LLM 响应为空：流式传输完成但未收到有效内容 [orchestrator/${this.config.model}/${this.config.provider}]`);
        }
      }
      finalText = sanitizeMissionOutcomeProtocolText(finalText);
      if (!runtimeShadow && this.isTerminationShadowEnabled()) {
        runtimeShadow = {
          enabled: true,
          reason: terminationReason,
          consistent: true,
          note: 'shadow-fallback',
        };
      }

      // 某些治理分支会在循环外合成 finalText（例如摘要劫持终止、工具结果降级文案），
      // 这些文本未经过流式管道时，前端会表现为“工具后停住但无结论”。
      if (!isTransientSystemCall && finalText.trim() && !finalTextDelivered && !this.abortController?.signal.aborted) {
        const fallbackStreamId = this.normalizer.startStream(this.currentTraceId!);
        this.normalizer.processTextDelta(fallbackStreamId, finalText);
        this.normalizer.endStream(fallbackStreamId);
        this.emit('message', finalText);
        decisionTrace.push(this.createDecisionTraceEntry({
          round: loopRounds,
          phase: 'finalize',
          action: 'fallback',
          requiredTotal: latestSnapshot?.requiredTotal ?? 0,
          reason: terminationReason,
          candidates: [terminationReason],
          note: 'final_text_fallback_stream',
        }));
      }

      this.lastRuntimeState = {
        reason: terminationReason,
        rounds: loopRounds,
        snapshot: latestSnapshot,
        shadow: runtimeShadow,
        decisionTrace,
        nextSteps: latestOutcomeSteps,
      };
      return finalText || '任务已中断';
    } catch (error: any) {
      // abort 中断不视为错误
      if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
        this.lastRuntimeState = {
          reason: 'external_abort',
          rounds: 0,
          shadow: this.isTerminationShadowEnabled()
            ? {
                enabled: true,
                reason: 'external_abort',
                consistent: true,
                note: 'shadow-abort',
              }
            : undefined,
          nextSteps: [],
        };
        return '任务已中断';
      }
      throw error;
    }
  }

  private createTerminationCandidate(
    reason: Exclude<OrchestratorTerminationReason, 'unknown'>,
    label: string
  ): TerminationCandidate {
    return {
      reason,
      eventId: `${label}_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
      triggeredAt: Date.now(),
    };
  }

  private isTerminationShadowEnabled(): boolean {
    const flag = (process.env.MAGI_TERMINATION_SHADOW || '').trim().toLowerCase();
    return flag === '1' || flag === 'true' || flag === 'on';
  }

  private buildShadowTerminationResult(params: {
    decisionEngine: OrchestratorDecisionEngine;
    snapshot: TerminationSnapshot;
    budget: OrchestratorExecutionBudget;
    noProgressStreak: number;
    consecutiveUpstreamModelErrors: number;
    budgetBreachStreak: number;
    externalWaitBreachStreak: number;
    primaryReason: Exclude<OrchestratorTerminationReason, 'unknown'>;
    assistantText: string;
  }): {
    enabled: boolean;
    reason: Exclude<OrchestratorTerminationReason, 'unknown'>;
    consistent: boolean;
    note?: string;
  } | undefined {
    if (!this.isTerminationShadowEnabled()) {
      return undefined;
    }

    const {
      snapshot,
      budget,
      noProgressStreak,
      consecutiveUpstreamModelErrors,
      budgetBreachStreak,
      externalWaitBreachStreak,
      primaryReason,
      assistantText,
      decisionEngine,
    } = params;
    const gateState = this.buildGateState(
      noProgressStreak,
      consecutiveUpstreamModelErrors,
      budgetBreachStreak,
      externalWaitBreachStreak,
    );
    const shadowReason = decisionEngine.resolveShadowReason({
      snapshot,
      budget,
      gateState,
      assistantText,
    });

    const consistent = shadowReason === primaryReason;
    if (!consistent) {
      logger.warn('Orchestrator.Termination.Shadow.不一致', {
        primaryReason,
        shadowReason,
        snapshotId: snapshot.snapshotId,
        attemptSeq: snapshot.attemptSeq,
      }, LogCategory.LLM);
    } else {
      logger.debug('Orchestrator.Termination.Shadow.一致', {
        reason: primaryReason,
        snapshotId: snapshot.snapshotId,
        attemptSeq: snapshot.attemptSeq,
      }, LogCategory.LLM);
    }

    return {
      enabled: true,
      reason: shadowReason,
      consistent,
      note: consistent ? undefined : `primary=${primaryReason}; shadow=${shadowReason}`,
    };
  }

  private buildGateState(
    noProgressStreak: number,
    consecutiveUpstreamModelErrors: number,
    budgetBreachStreak: number,
    externalWaitBreachStreak: number,
  ): OrchestratorGateState {
    return {
      noProgressStreak,
      consecutiveUpstreamModelErrors,
      budgetBreachStreak,
      externalWaitBreachStreak,
    };
  }

  private createDecisionTraceEntry(params: {
    round: number;
    phase: OrchestratorDecisionTraceEntry['phase'];
    action: OrchestratorDecisionTraceEntry['action'];
    requiredTotal: number;
    reason?: Exclude<OrchestratorTerminationReason, 'unknown'>;
    candidates?: string[];
    gateState?: OrchestratorGateState;
    note?: string;
  }): OrchestratorDecisionTraceEntry {
    return {
      round: params.round,
      phase: params.phase,
      action: params.action,
      requiredTotal: params.requiredTotal,
      reason: params.reason,
      candidates: params.candidates,
      gateState: params.gateState ? {
        noProgressStreak: params.gateState.noProgressStreak,
        budgetBreachStreak: params.gateState.budgetBreachStreak,
        externalWaitBreachStreak: params.gateState.externalWaitBreachStreak,
        consecutiveUpstreamModelErrors: params.gateState.consecutiveUpstreamModelErrors,
      } : undefined,
      note: params.note,
      timestamp: Date.now(),
    };
  }

  private logGateEvents(events: OrchestratorGateEvent[]): void {
    for (const event of events) {
      if (event.gate === 'budget') {
        logger.warn('Orchestrator.Termination.Gate.预算门禁触发', {
          ...event.payload,
          hardBudgetBreach: event.hard,
          triggerLabel: event.label,
        }, LogCategory.LLM);
      } else if (event.gate === 'external_wait') {
        logger.warn('Orchestrator.Termination.Gate.外部等待门禁触发', {
          ...event.payload,
          hardExternalWaitBreach: event.hard,
          triggerLabel: event.label,
        }, LogCategory.LLM);
      } else {
        logger.warn('Orchestrator.Termination.Gate.通用门禁触发', {
          gate: event.gate,
          ...event.payload,
          triggerLabel: event.label,
        }, LogCategory.LLM);
      }
    }
  }

  private resolveProtocolViolationTermination(input: {
    toolResults: ToolResult[];
    snapshot: TerminationSnapshot;
  }): {
    label: string;
    errorCodes: string[];
  } | null {
    const errorCodes = input.toolResults
      .map((result) => (result.standardized?.errorCode || '').trim().toLowerCase())
      .filter(Boolean);
    if (errorCodes.length === 0) {
      return null;
    }

    if (errorCodes.includes('orchestration_worker_wait_without_active_batch')) {
      return {
        label: 'orchestration_worker_wait_without_active_batch',
        errorCodes,
      };
    }

    if (
      input.snapshot.requiredTotal === 0
      && errorCodes.includes('orchestration_worker_wait_unknown_tasks')
    ) {
      return {
        label: 'orchestration_worker_wait_unknown_tasks_no_required_todos',
        errorCodes,
      };
    }

    return null;
  }

  private resolveProtocolViolationRecovery(input: {
    toolResults: ToolResult[];
    snapshot: TerminationSnapshot;
    userMessage: string;
    recoveryCount: number;
  }): {
    prompt: string;
    label: string;
    errorCodes: string[];
    nextRecoveryCount: number;
  } | null {
    const errorCodes = input.toolResults
      .map((result) => (result.standardized?.errorCode || '').trim().toLowerCase())
      .filter(Boolean);
    if (!errorCodes.includes('orchestration_worker_wait_without_active_batch')) {
      return null;
    }
    if (input.recoveryCount >= 1 || input.snapshot.requiredTotal > 0) {
      return null;
    }
    if (this.hasToolResultNamed(input.toolResults, 'worker_dispatch')) {
      return null;
    }
    if (this.userForbidsWorkerDispatchRetry(input.userMessage)) {
      return null;
    }

    return {
      prompt: buildWorkerWaitPreconditionRecoveryPrompt(),
      label: 'worker_wait_without_dispatch_result_recovery',
      errorCodes,
      nextRecoveryCount: input.recoveryCount + 1,
    };
  }

  private hasToolResultNamed(toolResults: ToolResult[], toolName: string): boolean {
    return toolResults.some((result) => (result.standardized?.toolName || '').trim() === toolName);
  }

  private detectPseudoOrchestrationToolNarration(text: string): boolean {
    const normalized = typeof text === 'string' ? text.trim() : '';
    if (!normalized) {
      return false;
    }
    const mentionCount = this.countOrchestrationToolMentions(normalized);
    if (mentionCount >= 2) {
      return true;
    }
    return /(调用|call|invoke|派发|dispatch).{0,20}(worker_dispatch|worker_wait)|(worker_dispatch|worker_wait).{0,20}(调用|call|invoke|派发|dispatch)/i.test(normalized);
  }

  private countOrchestrationToolMentions(text: string): number {
    if (!text || typeof text !== 'string') {
      return 0;
    }
    return (text.match(/worker_dispatch|worker_wait/gi) || []).length;
  }

  private userExplicitlyRequestsOrchestration(userMessage: string): boolean {
    const normalized = typeof userMessage === 'string' ? userMessage.trim() : '';
    if (!normalized) {
      return false;
    }
    return hasExplicitWorkerDispatchIntent(normalized);
  }

  private userForbidsWorkerDispatchRetry(userMessage: string): boolean {
    const normalized = userMessage.trim();
    if (!normalized) {
      return false;
    }

    const hasErrorClause = /如果.*错误|若.*错误|遇到错误|出错后|on error|if .*error/i.test(normalized);
    const forbidsRetryDispatch = /不要再调用\s*worker_dispatch|不要再\s*worker_dispatch|不允许再调用\s*worker_dispatch|do not call worker_dispatch again|must immediately end|stop immediately|立即结束|直接结束|立即失败/i.test(normalized);
    return hasErrorClause && forbidsRetryDispatch;
  }

  private buildRoundStreamRecoveryPrompt(input: {
    hasAccumulatedText: boolean;
    interruptedAfterToolSignal: boolean;
    recoveryAttempt: number;
    maxRecoveryAttempts: number;
    accumulatedText: string;
  }): string {
    const { hasAccumulatedText, interruptedAfterToolSignal, recoveryAttempt, maxRecoveryAttempts, accumulatedText } = input;
    if (interruptedAfterToolSignal && hasAccumulatedText) {
      return t('stream.recovery.toolCallWithText', { attempt: recoveryAttempt, max: maxRecoveryAttempts });
    }
    if (interruptedAfterToolSignal) {
      return t('stream.recovery.toolCallOnly', { attempt: recoveryAttempt, max: maxRecoveryAttempts });
    }
    // 纯文本中断：使用尾部锚定提示词
    return buildStreamRecoveryPrompt(t, accumulatedText, recoveryAttempt, maxRecoveryAttempts);
  }

  private buildToolRoundSignature(toolCalls: ToolCall[]): string {
    return toolCalls.map((toolCall) => {
      const args = toolCall.arguments && typeof toolCall.arguments === 'object'
        ? toolCall.arguments
        : {};
      const serializedArgs = Object.keys(args)
        .sort()
        .map((key) => `${key}:${this.toStableSignatureValue((args as Record<string, unknown>)[key])}`)
        .join(',');
      return `${toolCall.name}(${serializedArgs})`;
    }).join('|');
  }

  private toStableSignatureValue(value: unknown): string {
    if (value == null) {
      return '';
    }
    if (typeof value === 'string') {
      return value.trim().replace(/\s+/g, ' ').toLowerCase().slice(0, 120);
    }
    if (typeof value === 'number' || typeof value === 'boolean') {
      return String(value);
    }
    if (Array.isArray(value)) {
      return value.map(item => this.toStableSignatureValue(item)).join(',');
    }
    if (typeof value === 'object') {
      try {
        return JSON.stringify(value);
      } catch {
        return '[object]';
      }
    }
    return String(value);
  }

  private async buildTerminationSnapshot(params: {
    round: number;
    loopStartAt: number;
    loopStartTokenUsed: number;
    toolFailureRounds: number;
    baseline: CriticalPathBaseline | null;
    previousSnapshot: TerminationSnapshot | null;
  }): Promise<{
    snapshot: TerminationSnapshot;
    baseline: CriticalPathBaseline | null;
    cpRebased: boolean;
    progressed: boolean;
    regressed: boolean;
  }> {
    const todos = await this.fetchTodosForTermination();
    const requiredTodos = todos.filter(todo => this.isRequiredTodo(todo));
    const terminalStatuses = new Set(['completed', 'failed', 'skipped']);

    const pathResult = this.computeCriticalPathBaseline(requiredTodos, params.baseline);
    const currentBaseline = pathResult.baseline;
    const cpResolvedWeight = currentBaseline
      ? Array.from(currentBaseline.nodeIds).reduce((sum, todoId) => {
        const todo = requiredTodos.find(item => item.id === todoId);
        if (!todo) {
          return sum;
        }
        const resolved = todo.status === 'completed' || (todo.status === 'skipped' && todo.waiverApproved === true);
        if (!resolved) {
          return sum;
        }
        return sum + (currentBaseline.nodeWeights.get(todoId) || 1);
      }, 0)
      : 0;

    let unresolvedBlockers = 0;
    let blockerScore = 0;
    let externalWaitOpen = 0;
    let maxExternalWaitAgeMs = 0;
    const now = Date.now();
    for (const todo of requiredTodos) {
      if (todo.status !== 'blocked') {
        continue;
      }
      const externalWait = this.isExternalWaitTodo(todo);
      const ageMs = todo.createdAt ? Math.max(0, now - todo.createdAt) : 0;
      if (externalWait) {
        externalWaitOpen += 1;
        if (ageMs > maxExternalWaitAgeMs) {
          maxExternalWaitAgeMs = ageMs;
        }
        continue;
      }
      unresolvedBlockers += 1;
      blockerScore += this.computeBlockerScore(todo, ageMs);
    }

    const terminalRequiredTodos = requiredTodos.filter(todo => terminalStatuses.has(todo.status)).length;
    const runningRequired = requiredTodos.filter(todo => todo.status === 'running').length;
    const acceptedCriteria = requiredTodos.filter(todo => todo.status === 'completed').length;
    const failedRequired = requiredTodos.filter(todo => todo.status === 'failed').length;
    const runningOrPendingRequired = requiredTodos.filter(todo => !terminalStatuses.has(todo.status)).length;
    const totalTokens = this.getTotalTokenUsage();
    const currentTokenUsed = (totalTokens.inputTokens || 0) + (totalTokens.outputTokens || 0);
    // token 预算按“本次编排调用增量”计，不按 adapter 生命周期累计值计。
    // 否则会话用久后会出现“工具一返回就命中 budget_exceeded”的误停事故。
    const tokenUsed = Math.max(0, currentTokenUsed - Math.max(0, params.loopStartTokenUsed || 0));

    const progressVector: ProgressVector = {
      terminalRequiredTodos,
      acceptedCriteria,
      criticalPathResolved: currentBaseline && currentBaseline.totalWeight > 0
        ? cpResolvedWeight / currentBaseline.totalWeight
        : 0,
      unresolvedBlockers,
    };

    const snapshot: TerminationSnapshot = {
      snapshotId: `snap_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
      planId: this.currentTraceId || 'unknown-plan',
      attemptSeq: params.round,
      progressVector,
      reviewState: {
        accepted: acceptedCriteria,
        total: requiredTodos.length,
      },
      blockerState: {
        open: unresolvedBlockers,
        score: blockerScore,
        externalWaitOpen,
        maxExternalWaitAgeMs,
      },
      budgetState: {
        elapsedMs: now - params.loopStartAt,
        tokenUsed,
        errorRate: params.round > 0 ? params.toolFailureRounds / params.round : 0,
      },
      cpVersion: currentBaseline?.version || 1,
      requiredTotal: requiredTodos.length,
      failedRequired,
      runningOrPendingRequired,
      runningRequired,
      sourceEventIds: [],
      computedAt: now,
    };

    const progressEval = evaluateProgress(params.previousSnapshot, snapshot);
    const regressed = progressEval.regressed;
    const progressed = progressEval.progressed;

    // 当 required todo 缺失时，保守降级为全部 todo 口径，防止误判“已完成”
    if (requiredTodos.length === 0 && todos.length > 0) {
      const fallbackTerminal = todos.filter(todo => terminalStatuses.has(todo.status)).length;
      snapshot.requiredTotal = todos.length;
      snapshot.progressVector.terminalRequiredTodos = fallbackTerminal;
      snapshot.reviewState.total = todos.length;
      snapshot.runningOrPendingRequired = Math.max(0, todos.length - fallbackTerminal);
      snapshot.failedRequired = todos.filter(todo => todo.status === 'failed').length;
      snapshot.progressVector.acceptedCriteria = todos.filter(todo => todo.status === 'completed').length;
      snapshot.runningRequired = todos.filter(todo => todo.status === 'running').length;
    }

    return {
      snapshot,
      baseline: currentBaseline,
      cpRebased: pathResult.rebased,
      progressed,
      regressed,
    };
  }

  private async fetchTodosForTermination(): Promise<OrchestratorTodoSummary[]> {
    try {
      const orchestratorContext = this.toolManager.getSnapshotContext('orchestrator');
      const scopedMissionId = typeof orchestratorContext?.missionId === 'string'
        ? orchestratorContext.missionId.trim()
        : '';
      // 终止快照必须强制 mission 级作用域，禁止退化为 session 全量查询。
      // 否则历史任务会污染本轮判定，出现“工具返回后被误判 completed”。
      if (!scopedMissionId || scopedMissionId.startsWith('session:')) {
        return [];
      }
      const toolCall: ToolCall = {
        id: `internal_todo_list_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
        name: 'todo_list',
        arguments: { mission_id: scopedMissionId },
      };
      const result = await this.toolManager.execute(
        toolCall,
        this.abortController?.signal,
        { workerId: 'orchestrator', role: 'orchestrator' }
      );
      if (result.isError) {
        return [];
      }
      const parsed = JSON.parse(result.content || '[]');
      if (!Array.isArray(parsed)) {
        return [];
      }
      return this.toTodoSummaries(parsed);
    } catch (error: any) {
      logger.warn('Orchestrator.终止快照.获取Todos失败', {
        error: error?.message || String(error),
      }, LogCategory.LLM);
      return [];
    }
  }

  private toTodoSummaries(raw: any[]): OrchestratorTodoSummary[] {
    const summaries: OrchestratorTodoSummary[] = [];
    for (const item of raw) {
      if (!item || typeof item !== 'object' || typeof item.id !== 'string') {
        continue;
      }
      summaries.push({
        id: item.id,
        missionId: typeof item.missionId === 'string' ? item.missionId : undefined,
        assignmentId: typeof item.assignmentId === 'string' ? item.assignmentId : undefined,
        parentId: typeof item.parentId === 'string' ? item.parentId : undefined,
        content: typeof item.content === 'string' ? item.content : undefined,
        status: typeof item.status === 'string' ? item.status : 'pending',
        worker: typeof item.worker === 'string' ? item.worker : undefined,
        blockedReason: typeof item.blockedReason === 'string' ? item.blockedReason : undefined,
        approvalStatus: typeof item.approvalStatus === 'string' ? item.approvalStatus : undefined,
        dependsOn: Array.isArray(item.dependsOn) ? item.dependsOn.filter((dep: any) => typeof dep === 'string') : [],
        required: typeof item.required === 'boolean' ? item.required : true,
        effortWeight: typeof item.effortWeight === 'number' ? item.effortWeight : 1,
        waiverApproved: item.waiverApproved === true,
        createdAt: typeof item.createdAt === 'number' ? item.createdAt : undefined,
      });
    }
    return summaries;
  }

  private isRequiredTodo(todo: OrchestratorTodoSummary): boolean {
    return todo.required !== false;
  }

  private isExternalWaitTodo(todo: OrchestratorTodoSummary): boolean {
    if (todo.approvalStatus === 'pending') {
      return true;
    }
    const reason = (todo.blockedReason || '').toLowerCase();
    if (!reason) {
      return false;
    }
    return reason.includes('审批')
      || reason.includes('approval')
      || reason.includes('等待用户')
      || reason.includes('external')
      || reason.includes('等待外部');
  }

  private computeBlockerScore(todo: OrchestratorTodoSummary, ageMs: number): number {
    const reason = (todo.blockedReason || '').toLowerCase();
    let severityWeight = 2;
    if (reason.includes('critical') || reason.includes('致命') || reason.includes('不可恢复')) {
      severityWeight = 8;
    } else if (reason.includes('contract') || reason.includes('契约') || reason.includes('依赖')) {
      severityWeight = 4;
    } else if (reason.includes('warning') || reason.includes('提示')) {
      severityWeight = 1;
    }
    const ageMinutes = ageMs / 60000;
    return severityWeight * Math.log1p(Math.max(0, ageMinutes));
  }

  private computeCriticalPathBaseline(
    todos: OrchestratorTodoSummary[],
    previous: CriticalPathBaseline | null
  ): { baseline: CriticalPathBaseline | null; rebased: boolean } {
    if (!todos.length) {
      return { baseline: previous, rebased: false };
    }

    const current = this.buildCriticalPathFromTodos(todos);
    if (!current) {
      return { baseline: previous, rebased: false };
    }

    if (!previous || previous.totalWeight <= 0) {
      return {
        baseline: {
          ...current,
          version: 1,
        },
        rebased: true,
      };
    }

    const missingNodes = Array.from(previous.nodeIds).filter((id) => !todos.some((todo) => todo.id === id));
    const hasSplitSignal = missingNodes.some((missingId) => todos.some(todo => todo.parentId === missingId));
    const growthRatio = previous.pathWeight > 0
      ? (current.pathWeight - previous.pathWeight) / previous.pathWeight
      : 0;
    const shouldRebase = hasSplitSignal || growthRatio > OrchestratorLLMAdapter.CP_REBASE_THRESHOLD;

    if (!shouldRebase) {
      return { baseline: previous, rebased: false };
    }

    return {
      baseline: {
        ...current,
        version: previous.version + 1,
      },
      rebased: true,
    };
  }

  private buildCriticalPathFromTodos(todos: OrchestratorTodoSummary[]): Omit<CriticalPathBaseline, 'version'> | null {
    if (!todos.length) {
      return null;
    }

    const todoMap = new Map<string, OrchestratorTodoSummary>();
    const nodeWeights = new Map<string, number>();
    const indegree = new Map<string, number>();
    const children = new Map<string, string[]>();

    for (const todo of todos) {
      todoMap.set(todo.id, todo);
      const weight = typeof todo.effortWeight === 'number' && todo.effortWeight > 0
        ? todo.effortWeight
        : 1;
      nodeWeights.set(todo.id, weight);
      indegree.set(todo.id, 0);
      children.set(todo.id, []);
    }

    for (const todo of todos) {
      const deps = Array.isArray(todo.dependsOn) ? todo.dependsOn : [];
      for (const depId of deps) {
        if (!todoMap.has(depId)) {
          continue;
        }
        children.get(depId)!.push(todo.id);
        indegree.set(todo.id, (indegree.get(todo.id) || 0) + 1);
      }
    }

    const queue: string[] = [];
    for (const [id, degree] of indegree.entries()) {
      if (degree === 0) {
        queue.push(id);
      }
    }

    const topo: string[] = [];
    while (queue.length > 0) {
      const currentId = queue.shift()!;
      topo.push(currentId);
      for (const childId of children.get(currentId) || []) {
        const nextDegree = (indegree.get(childId) || 0) - 1;
        indegree.set(childId, nextDegree);
        if (nextDegree === 0) {
          queue.push(childId);
        }
      }
    }

    if (topo.length !== todos.length) {
      const allNodeIds = new Set(todos.map(todo => todo.id));
      const totalWeight = Array.from(nodeWeights.values()).reduce((sum, value) => sum + value, 0);
      return {
        nodeIds: allNodeIds,
        nodeWeights,
        totalWeight,
        pathWeight: totalWeight,
      };
    }

    const dist = new Map<string, number>();
    const prev = new Map<string, string | undefined>();
    for (const id of topo) {
      dist.set(id, nodeWeights.get(id) || 1);
      prev.set(id, undefined);
    }

    for (const id of topo) {
      const base = dist.get(id) || 0;
      for (const childId of children.get(id) || []) {
        const candidate = base + (nodeWeights.get(childId) || 1);
        if (candidate > (dist.get(childId) || 0)) {
          dist.set(childId, candidate);
          prev.set(childId, id);
        }
      }
    }

    let targetId = topo[0];
    let maxDist = dist.get(targetId) || 0;
    for (const id of topo) {
      const value = dist.get(id) || 0;
      if (value > maxDist) {
        maxDist = value;
        targetId = id;
      }
    }

    const nodeIds = new Set<string>();
    let cursor: string | undefined = targetId;
    while (cursor) {
      nodeIds.add(cursor);
      cursor = prev.get(cursor);
    }

    const totalWeight = Array.from(nodeIds).reduce((sum, id) => sum + (nodeWeights.get(id) || 1), 0);
    return {
      nodeIds,
      nodeWeights,
      totalWeight,
      pathWeight: maxDist,
    };
  }

  /**
   * 执行工具调用
   */
  private async executeToolCalls(
    toolCalls: ToolCall[],
  ) {
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

      // 参数解析失败：不执行工具，直接回传给模型修正参数
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
        this.emit('toolResult', toolCall.name, errorContent);
        continue;
      }

      try {
        const rawResult = await this.toolManager.execute(
          toolCall,
          this.abortController?.signal,
          { workerId: 'orchestrator', role: 'orchestrator' },
        );
        this.truncateToolResultContent(toolCall, rawResult, maxToolResultChars);
        const result = this.ensureStandardizedToolResult(toolCall, rawResult, toolSourceMap);
        results.push(result);
        this.emit('toolResult', toolCall.name, result.content);
      } catch (error: any) {
        results.push(this.createSyntheticToolResult(
          toolCall,
          `Error: ${error?.message || String(error)}`,
          'error',
          toolSourceMap,
        ));
      }
    }

    return results;
  }

}
