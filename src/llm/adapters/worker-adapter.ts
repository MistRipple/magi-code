/**
 * Worker LLM 适配器
 * 用于 Worker 代理（claude, codex, gemini）
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig, WorkerSlot } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage, ToolCall } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';
import { ProfileLoader } from '../../orchestrator/profile/profile-loader';
import { GuidanceInjector } from '../../orchestrator/profile/guidance-injector';

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
    stallWarnLevel3: 18,
    stallAbortThreshold: 25,
    maxTotalRounds: 40,
    noOutputWarn: 5,
    noOutputForce: 8,
    noOutputAbort: 12,
  },
  codex: {
    consecutiveFailThreshold: 5,
    totalFailLimit: 15,
    stallWarnLevel1: 3,
    stallWarnLevel2: 6,
    stallWarnLevel3: 10,
    stallAbortThreshold: 15,
    maxTotalRounds: 25,
    noOutputWarn: 3,
    noOutputForce: 5,
    noOutputAbort: 8,
  },
  gemini: {
    consecutiveFailThreshold: 5,
    totalFailLimit: 25,
    stallWarnLevel1: 5,
    stallWarnLevel2: 10,
    stallWarnLevel3: 18,
    stallAbortThreshold: 25,
    maxTotalRounds: 40,
    noOutputWarn: 5,
    noOutputForce: 8,
    noOutputAbort: 12,
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
}

/**
 * Worker LLM 适配器
 */
export class WorkerLLMAdapter extends BaseLLMAdapter {
  private workerSlot: WorkerSlot;
  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private profileLoader: ProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private historyConfig: Required<HistoryManagementConfig>;
  private stallConfig: StallDetectionConfig;
  private seenThinking = false;
  private decisionHookAppliedForThinking = false;
  /** 工具摘要是否已注入到 systemPrompt（lazy init，仅执行一次） */
  private toolsSummaryInjected = false;

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
    this.currentTraceId = this.generateTraceId();

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

    // 获取工具定义（Worker 过滤掉编排工具，编排权限仅属于 Orchestrator）
    const ORCHESTRATION_TOOLS = ['dispatch_task', 'send_worker_message'];
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
      // 无实质文本输出检测状态
      let noSubstantiveOutputRounds = 0;      // 连续无实质输出轮次
      let lastNoOutputWarnLevel = 0;          // 上次警告级别

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();

      let round = 0;
      while (true) {
        // 中断检查：每轮迭代入口检测 abort 信号
        if (this.abortController.signal.aborted) {
          break;
        }

        // 总轮次安全网：防止任何场景下的无限循环
        if (round >= sc.maxTotalRounds) {
          logger.warn(`${this.agent} 达到总轮次上限`, { round }, LogCategory.LLM);
          finalText = finalText || `已执行 ${round} 轮工具调用，达到安全上限，任务终止。请检查任务是否需要拆分。`;
          break;
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
          tools: toolDefinitions.length > 0 ? toolDefinitions : undefined,
          stream: true,
          maxTokens: 4096,
          temperature: 0.7,
          signal: this.abortController.signal,
        };

        let accumulatedText = '';
        let toolCalls: ToolCall[] = [];

        try {
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              if (this.seenThinking && !this.decisionHookAppliedForThinking) {
                this.decisionHookAppliedForThinking = true;
                this.applyDecisionHook({ type: 'thinking' });
              }
              accumulatedText += chunk.content;
              this.normalizer.processTextDelta(streamId, chunk.content);
              this.emit('message', chunk.content);
            } else if (chunk.type === 'thinking' && chunk.thinking) {
              this.normalizer.processThinking(streamId, chunk.thinking);
              this.emit('thinking', chunk.thinking);
              this.seenThinking = true;
            } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
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

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            this.conversationHistory.push({ role: 'assistant', content: assistantText });
            finalText = assistantText;
            this.normalizer.endStream(streamId);
            break;
          }

          // 有工具调用 → 同步到当轮 stream，执行工具，endStream 后进入下一轮
          for (const toolCall of toolCalls) {
            this.normalizer.addToolCall(streamId, {
              type: 'tool_call',
              toolName: toolCall.name,
              toolId: toolCall.id,
              status: 'running',
              input: JSON.stringify(toolCall.arguments, null, 2),
            });
          }

          const assistantContent: any[] = [];
          if (assistantText) {
            assistantContent.push({ type: 'text', text: assistantText });
          }
          for (const toolCall of toolCalls) {
            assistantContent.push({
              type: 'tool_use',
              id: toolCall.id,
              name: toolCall.name,
              input: toolCall.arguments,
            });
          }
          this.conversationHistory.push({ role: 'assistant', content: assistantContent });

          const toolResults = await this.executeToolCalls(toolCalls);

          // 中断检查：工具执行完成后立即检测 abort，跳过后续处理直接退出循环
          if (this.abortController?.signal.aborted) {
            this.normalizer.endStream(streamId);
            break;
          }

          for (const result of toolResults) {
            this.normalizer.finishToolCall(
              streamId,
              result.toolCallId,
              result.isError ? undefined : result.content,
              result.isError ? result.content : undefined,
            );
          }

          this.conversationHistory.push({
            role: 'user',
            content: toolResults.map((result) => ({
              type: 'tool_result',
              tool_use_id: result.toolCallId,
              content: result.content,
              is_error: result.isError,
            })),
          });

          // 连续失败检测
          const allFailed = toolResults.every(r => r.isError);
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

              // 根据探索度计算空转增量
              // 查看新文件 = 有目的探索（低增量），反复查看旧文件 = 空转（高增量）
              const newRatio = roundPaths.length > 0 ? newPaths.length / roundPaths.length : 0;
              const stallIncrement = newRatio >= 0.5 ? 0.5 : 1.5;
              readOnlyStallScore += stallIncrement;

              // 多级渐进式警告
              if (readOnlyStallScore >= sc.stallAbortThreshold) {
                finalText = assistantText || `连续 ${readOnlyConsecutiveRounds} 轮仅调用只读工具（空转分数 ${readOnlyStallScore.toFixed(1)}），已查看 ${visitedPaths.size} 个文件，判定为搜索空转终止。`;
                logger.warn(`${this.agent} 空转终止`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.normalizer.endStream(streamId);
                break;
              }

              if (readOnlyStallScore >= sc.stallWarnLevel3 && lastStallWarnLevel < 3) {
                lastStallWarnLevel = 3;
                logger.warn(`${this.agent} 空转最终警告`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] ⚠️ 最终警告：你已连续 ${readOnlyConsecutiveRounds} 轮仅使用只读工具（已查看 ${visitedPaths.size} 个不同文件）。如果下一轮仍不使用 text_editor 的 write 命令修改代码，任务将被强制终止。请立即动手修改。`,
                });
              } else if (readOnlyStallScore >= sc.stallWarnLevel2 && lastStallWarnLevel < 2) {
                lastStallWarnLevel = 2;
                logger.warn(`${this.agent} 空转二级警告`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] 你已连续 ${readOnlyConsecutiveRounds} 轮仅使用搜索/查看类工具，已查看 ${visitedPaths.size} 个不同文件。你收集的信息已经足够，请立即使用 text_editor 的 write 命令开始修改代码。不要再查看文件。`,
                });
              } else if (readOnlyStallScore >= sc.stallWarnLevel1 && lastStallWarnLevel < 1) {
                lastStallWarnLevel = 1;
                logger.info(`${this.agent} 空转一级提醒`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] 你已连续 ${readOnlyConsecutiveRounds} 轮仅使用只读工具查看代码（已查看 ${visitedPaths.size} 个文件）。请考虑开始使用 text_editor 修改代码来推进任务。`,
                });
              }
            } else {
              // 包含写入操作 → 重置空转状态
              readOnlyStallScore = 0;
              readOnlyConsecutiveRounds = 0;
              lastStallWarnLevel = 0;
              // 注意：visitedPaths 不重置，保持全局去重
            }
          }

          // 无实质文本输出检测：Worker 不断调用工具但不给用户产出可见内容
          // （与只读空转检测互补，覆盖 execute+search 混合循环的场景）
          const SUBSTANTIVE_TEXT_THRESHOLD = 20;
          if (accumulatedText.trim().length < SUBSTANTIVE_TEXT_THRESHOLD) {
            noSubstantiveOutputRounds++;

            if (noSubstantiveOutputRounds >= sc.noOutputAbort) {
              finalText = accumulatedText || `连续 ${noSubstantiveOutputRounds} 轮未产出实质性文本内容，仅调用工具。任务终止，请检查任务描述是否足够明确。`;
              logger.warn(`${this.agent} 无实质输出终止`, { rounds: noSubstantiveOutputRounds, totalRound: round }, LogCategory.LLM);
              this.normalizer.endStream(streamId);
              break;
            }

            if (noSubstantiveOutputRounds >= sc.noOutputForce && lastNoOutputWarnLevel < 2) {
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
          this.normalizer.endStream(streamId, error?.message || 'Request failed');
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
        throw new Error('LLM 响应为空：流式传输完成但未收到有效内容');
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
      this.abortController = undefined;
    }
    this.setState(AdapterState.CONNECTED);
    logger.info(`${this.agent} adapter interrupted`, undefined, LogCategory.LLM);
  }

  /**
   * 清除对话历史
   */
  clearHistory(): void {
    this.conversationHistory = [];
    logger.debug(`${this.agent} conversation history cleared`, undefined, LogCategory.LLM);
  }

  /**
   * 设置系统提示
   */
  setSystemPrompt(prompt: string): void {
    this.systemPrompt = prompt;
    logger.debug(`${this.agent} system prompt updated`, undefined, LogCategory.LLM);
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

    for (const toolCall of toolCalls) {
      // 中断检查：工具调用之间检测 abort 信号，避免中断后继续执行后续工具
      if (this.abortController?.signal.aborted) {
        results.push({
          toolCallId: toolCall.id,
          content: '任务已中断',
          isError: true,
        });
        continue;
      }

      try {
        logger.debug(`Executing tool: ${toolCall.name}`, { args: toolCall.arguments }, LogCategory.TOOLS);

        const result = await this.toolManager.execute(toolCall, this.abortController?.signal);
        if (typeof result.content === 'string' && result.content.length > maxToolResultChars) {
          const truncated = result.content.slice(0, maxToolResultChars);
          result.content = `${truncated}\n...[truncated ${result.content.length - maxToolResultChars} chars]`;
        }
        results.push(result);

        this.emit('toolResult', toolCall.name, result.content);

        logger.debug(`Tool execution completed: ${toolCall.name}`, {
          success: !result.isError,
        }, LogCategory.TOOLS);
      } catch (error: any) {
        logger.error(`Tool execution failed: ${toolCall.name}`, {
          error: error.message,
        }, LogCategory.TOOLS);

        results.push({
          toolCallId: toolCall.id,
          content: `Error: ${error.message}`,
          isError: true,
        });
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
   * 保留最近的 N 轮对话
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
      this.conversationHistory = this.conversationHistory.slice(-preserveCount);
      logger.debug(`${this.agent} history truncated`, {
        removedMessages: truncatedCount,
        remainingMessages: this.conversationHistory.length,
        previousChars: currentChars,
        currentChars: this.getHistoryChars(),
      }, LogCategory.LLM);
    }
  }

  private normalizeHistoryForTools(): void {
    if (this.conversationHistory.length === 0) {
      return;
    }

    const cleaned: LLMMessage[] = [];
    for (let i = 0; i < this.conversationHistory.length; i++) {
      const msg = this.conversationHistory[i];

      if (this.hasToolUse(msg)) {
        const next = this.conversationHistory[i + 1];
        const prev = cleaned[cleaned.length - 1];
        if (!this.isToolResultUser(next) || !this.isUserOrToolResult(prev)) {
          continue;
        }
        cleaned.push(msg);
        cleaned.push(next);
        i += 1;
        continue;
      }

      if (this.isToolResultUser(msg)) {
        const prev = this.conversationHistory[i - 1];
        if (!this.hasToolUse(prev)) {
          continue;
        }
      }

      cleaned.push(msg);
    }

    this.conversationHistory = cleaned;
  }

  private isUserOrToolResult(message?: LLMMessage): boolean {
    if (!message) {
      return false;
    }
    if (message.role === 'user') {
      return true;
    }
    return this.isToolResultUser(message);
  }

  private hasToolUse(message?: LLMMessage): boolean {
    if (!message || !Array.isArray(message.content)) {
      return false;
    }
    return message.content.some((block: any) => block?.type === 'tool_use');
  }

  private isToolResultUser(message?: LLMMessage): boolean {
    if (!message || message.role !== 'user' || !Array.isArray(message.content)) {
      return false;
    }
    return message.content.some((block: any) => block?.type === 'tool_result');
  }

  /**
   * 判断工具调用是否为只读操作（搜索/检索/查看类）
   *
   * 用于智能空转检测：连续多轮只调用只读工具而无写入操作时，
   * 根据探索度计算空转分数，渐进式注入提示推动 Worker 开始行动。
   */
  private isReadOnlyToolCall(toolCall: ToolCall): boolean {
    const name = toolCall.name;

    // 明确的只读内置工具
    const READ_ONLY_BUILTINS = [
      'codebase_retrieval',
      'grep_search',
      'list-processes',
      'read-process',
      'web_search',
      'web_fetch',
      'mermaid_diagram',
    ];
    if (READ_ONLY_BUILTINS.includes(name)) {
      return true;
    }

    // text_editor：view/list 命令是只读，write/create/undo_edit 是写入
    if (name === 'text_editor') {
      const command = toolCall.arguments?.command;
      return command === 'view' || command === 'list';
    }

    // MCP 工具：通过名称模式判断（搜索/检索/读取/查看类）
    const READ_ONLY_PATTERNS = /retrieval|search|read|fetch|view|get[_-]|list[_-]|query|deepwiki|resolve/i;
    if (READ_ONLY_PATTERNS.test(name)) {
      return true;
    }

    // 其他工具视为写入操作
    return false;
  }

  /**
   * 从一批工具调用中提取访问的文件路径（用于空转探索度判定）
   *
   * 提取逻辑：
   * - text_editor view → arguments.path
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
}
