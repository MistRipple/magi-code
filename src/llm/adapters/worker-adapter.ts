/**
 * Worker LLM 适配器
 * 用于 Worker 代理（claude, codex, gemini）
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig, WorkerSlot } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage, ToolCall, sanitizeToolOrder } from '../types';
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
    noOutputWarn: 5,
    noOutputForce: 8,
    noOutputAbort: 12,
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
  /** 检索结果缓存：防止模型换措辞重复搜索相同内容 */
  private searchResultCache = new Map<string, string>();
  /** 已读取过的文件路径（文件级去重：同一文件只需读取一次） */
  private viewedFiles = new Set<string>();
  /**
   * 滚动上下文摘要：截断时从被丢弃消息中提取的关键信息
   *
   * 每次 truncateHistoryIfNeeded 触发截断时，被丢弃消息的精华会合并到此摘要中。
   * 此摘要以 user 角色消息注入到对话历史开头，确保 LLM 不丢失前期关键发现。
   */
  private rollingContextSummary: string | null = null;
  /** 滚动摘要最大字符数（约 500 tokens） */
  private static readonly MAX_ROLLING_SUMMARY_CHARS = 2000;
  /** L1+: 按文件路径记录已读取的 view_range 集合（分段精确去重 + 碎片化预警） */
  private viewedRanges = new Map<string, Set<string>>();
  /** 当前任务内的去重命中总次数（递增惩罚用） */
  private totalDedupHits = 0;
  /** 当前轮次的去重命中次数（空转分数加权用） */
  private roundDedupHits = 0;
  /** 失败写操作缓存：防止模型反复重试相同的失败写操作 */
  private failedWriteCache = new Map<string, { count: number; error: string }>();
  /** 成功写操作缓存：防止模型反复执行完全相同的已成功写操作 */
  private successWriteCache = new Set<string>();
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
    this.historyConfig = {
      maxMessages: adapterConfig.historyConfig?.maxMessages ?? 50,
      maxChars: adapterConfig.historyConfig?.maxChars ?? 100000,
      preserveRecentRounds: adapterConfig.historyConfig?.preserveRecentRounds ?? 5,
    };

    // 注册基于意图的大模型编辑回调给工具管理器
    this.toolManager.setLlmEditHandler(async (filePath, fileContent, summary, detailedDesc) => {
      return this.handleFileEditWithLLM(filePath, fileContent, summary, detailedDesc);
    });
  }

  /**
   * 使用大模型处理文件编辑意图，返回修改后的完整代码
   */
  private async handleFileEditWithLLM(filePath: string, fileContent: string, summary: string, detailedDesc: string): Promise<string> {
    const prompt = `你是一个专业的代码编辑 Agent。
你的任务是将用户的修改意图准确地应用到以下文件中。

目标文件路径: ${filePath}

当前文件内容:
\`\`\`
${fileContent}
\`\`\`

修改摘要 (Summary): ${summary}
详细描述 (Detailed Intent): ${detailedDesc}

请输出修改后的完整文件内容，必须包裹在 \`\`\` 代码块中。不要添加任何多余的解释说明、不要在代码块外部添加 markdown。
如果你认为没有必要修改，或者意图无法应用，请直接输出原内容。`;

    const messages: LLMMessage[] = [{ role: 'user', content: prompt }];

    // 不带工具、低温度，专注于单次代码编辑
    const response = await this.client.sendMessage({
      messages,
      systemPrompt: '你是一个严格的编辑器程序，你的唯一职责是输出被编辑后的文件内容，严格遵守用户的意图。',
      temperature: 0.1,
      stream: false
    });

    const output = response.content;

    // 提取代码块内的内容（标准闭合情况）
    // 兼容 \r\n 换行以及代码块结尾无空行的情况
    const codeBlockMatch = output.match(/```[a-zA-Z]*\r?\n([\s\S]*?)\r?\n```/);
    if (codeBlockMatch) {
      return codeBlockMatch[1];
    }

    // 容错：处理模型由于长度限制等原因未输出结尾 ``` 的截断情况
    const unclosedMatch = output.match(/```[a-zA-Z]*\r?\n([\s\S]*)$/);
    if (unclosedMatch) {
      // 如果截断内容尾部恰好有不完整的 ``` 标记，清理掉
      return unclosedMatch[1].replace(/\r?\n```\s*$/, '');
    }

    // 最后兜底：若没有任何 markdown 代码块标记，直接返回原始输出
    return output.trim();
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
      // 重复无效 launch-process 拦截计数（避免同一错误参数反复刷屏）
      let repeatedLaunchProcessInterceptRounds = 0;

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
          if (!forceNoToolsNextRound) {
            forceNoToolsNextRound = true;
            logger.warn(`${this.agent} 达到总轮次上限，触发强制总结`, { round }, LogCategory.LLM);
            this.conversationHistory.push({
              role: 'user',
              content: `[System] 你已执行 ${round} 轮工具调用，达到系统上限。工具调用能力已被收回。请立即总结当前进展和执行结果。`,
            });
            continue; // 进入下一轮（无工具），让模型产出总结
          }
          logger.warn(`${this.agent} 达到总轮次上限`, { round }, LogCategory.LLM);
          finalText = finalText || `已执行 ${round} 轮工具调用，达到安全上限，任务终止。`;
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
          tools: forceNoToolsNextRound ? undefined : (toolDefinitions.length > 0 ? toolDefinitions : undefined),
          stream: true,
          maxTokens: 4096,
          temperature: 0.7,
          signal: this.abortController.signal,
        };

        let accumulatedText = '';
        let hasStreamedTextDelta = false;
        let toolCalls: ToolCall[] = [];

        try {
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              this.normalizer.processTextDelta(streamId, chunk.content);
              hasStreamedTextDelta = true;
              if (this.seenThinking && !this.decisionHookAppliedForThinking) {
                this.decisionHookAppliedForThinking = true;
                this.applyDecisionHook({ type: 'thinking' });
              }
              accumulatedText += chunk.content;
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
          if (assistantText && !hasStreamedTextDelta) {
            // 兜底：部分 provider 可能仅在最终响应体返回文本，未逐块回调 content_delta。
            this.normalizer.processTextDelta(streamId, assistantText);
          }

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            if (assistantText && !hasStreamedTextDelta) {
              this.emit('message', assistantText);
            }
            this.conversationHistory.push({ role: 'assistant', content: assistantText });
            finalText = assistantText;
            this.normalizer.endStream(streamId);
            break;
          }

          // 有工具调用 → 只对无需授权的工具即时渲染卡片。
          // 高风险工具（需授权）延后渲染，确保授权提示先出现。
          const preAnnouncedToolCallIds = new Set<string>();
          for (const toolCall of toolCalls) {
            if (this.toolManager.requiresUserAuthorization(toolCall.name)) {
              continue;
            }
            preAnnouncedToolCallIds.add(toolCall.id);
            this.normalizer.addToolCall(streamId, {
              type: 'tool_call',
              toolName: toolCall.name,
              toolId: toolCall.id,
              status: 'running',
              input: JSON.stringify(toolCall.arguments, null, 2),
            });
          }

          const assistantContent: any[] = [];
          for (const toolCall of toolCalls) {
            assistantContent.push({
              type: 'tool_use',
              id: toolCall.id,
              name: toolCall.name,
              input: toolCall.arguments,
            });
          }
          this.conversationHistory.push({ role: 'assistant', content: assistantContent });

          this.roundDedupHits = 0;
          this.roundWriteInterceptCount = 0;
          const toolResults = await this.executeToolCalls(toolCalls);

          // 中断检查：工具执行完成后立即检测 abort，跳过后续处理直接退出循环
          if (this.abortController?.signal.aborted) {
            this.normalizer.endStream(streamId);
            break;
          }

          const toolCallMap = new Map(toolCalls.map((toolCall) => [toolCall.id, toolCall] as const));
          for (const result of toolResults) {
            const toolCall = toolCallMap.get(result.toolCallId);
            if (!toolCall) {
              continue;
            }
            if (preAnnouncedToolCallIds.has(result.toolCallId)) {
              this.normalizer.finishToolCall(
                streamId,
                result.toolCallId,
                result.isError ? undefined : result.content,
                result.isError ? result.content : undefined,
                result.fileChange,
                result.standardized
              );
              continue;
            }

            // 高风险工具：单独产出一张工具卡片，确保其时序晚于授权卡片
            const deferredToolStreamId = this.normalizer.startStream(this.currentTraceId!);
            this.normalizer.addToolCall(deferredToolStreamId, {
              type: 'tool_call',
              toolName: toolCall.name,
              toolId: toolCall.id,
              status: result.isError ? 'failed' : 'completed',
              input: JSON.stringify(toolCall.arguments, null, 2),
              output: result.isError ? undefined : result.content,
              error: result.isError ? result.content : undefined,
              standardized: result.standardized,
            });

            this.normalizer.finishToolCall(
              deferredToolStreamId,
              toolCall.id,
              result.isError ? undefined : result.content,
              result.isError ? result.content : undefined,
              result.fileChange,
              result.standardized
            );
            this.normalizer.endStream(deferredToolStreamId);
          }

          this.conversationHistory.push({
            role: 'user',
            content: toolResults.map((result) => ({
              type: 'tool_result',
              tool_use_id: result.toolCallId,
              content: result.content,
              is_error: result.isError,
              standardized: result.standardized,
            })),
          });

          const isLaunchProcessInterceptRound = toolCalls.length > 0
            && toolCalls.length === toolResults.length
            && toolCalls.every(tc => tc.name === 'launch-process')
            && toolResults.every(result => result.isError
              && typeof result.content === 'string'
              && result.content.includes('[系统拦截]'));

          if (isLaunchProcessInterceptRound) {
            repeatedLaunchProcessInterceptRounds++;
            if (repeatedLaunchProcessInterceptRounds >= 2 && !forceNoToolsNextRound) {
              forceNoToolsNextRound = true;
              this.conversationHistory.push({
                role: 'user',
                content: '[System] 你正在重复调用同一失败的 launch-process。下一轮禁止调用工具。请仅输出修正后的命令参数方案：cwd 必须使用工作区名或 "<工作区名>/相对路径"，不要使用 /home/user 这类固定系统路径。',
              });
            }
          } else {
            repeatedLaunchProcessInterceptRounds = 0;
          }

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
                logger.warn(`${this.agent} 空转达到最终引导阈值`, { rounds: readOnlyConsecutiveRounds, score: readOnlyStallScore, uniquePaths: visitedPaths.size }, LogCategory.LLM);
                this.conversationHistory.push({
                  role: 'user',
                  content: `[System] 你已连续 ${readOnlyConsecutiveRounds} 轮仅使用搜索/查看工具。你收集的信息已经完全足够。请立即开始修改代码或输出最终结论——不要再搜索。${visitedSummary}`,
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
              logger.warn(`${this.agent} 无实质输出达到最终引导`, { rounds: noSubstantiveOutputRounds, totalRound: round }, LogCategory.LLM);
              this.conversationHistory.push({
                role: 'user',
                content: `[System] 你已连续 ${noSubstantiveOutputRounds} 轮未产出面向用户的文本内容。请在完成当前操作后，输出你的执行进展和结果摘要。`,
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
    this.searchResultCache.clear();
    this.viewedFiles.clear();
    this.viewedRanges.clear();
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

      // 文件级去重：同一文件只需读取一次，阻断"对同一文件反复发起不同查询"的模式
      const fileDedup = this.checkFileAccessDuplicate(toolCall);
      if (fileDedup) {
        logger.info(`${this.agent} 文件级去重命中`, { tool: toolCall.name, path: toolCall.arguments?.path }, LogCategory.TOOLS);
        results.push(this.createSyntheticToolResult(
          toolCall,
          fileDedup,
          'success',
          toolSourceMap,
        ));
        this.emit('toolResult', toolCall.name, fileDedup);
        continue;
      }

      // 检索去重：只读工具的相似查询直接返回缓存结果，阻断无效循环
      const dedupResult = this.checkSearchDuplicate(toolCall);
      if (dedupResult) {
        logger.info(`${this.agent} 检索去重命中`, { tool: toolCall.name }, LogCategory.TOOLS);
        results.push(this.createSyntheticToolResult(
          toolCall,
          dedupResult,
          'success',
          toolSourceMap,
        ));
        this.emit('toolResult', toolCall.name, dedupResult);
        continue;
      }

      // 失败写操作去重：相同参数的写操作重复失败时短路拦截
      const failedWriteDedup = this.checkFailedWriteDuplicate(toolCall);
      if (failedWriteDedup) {
        logger.info(`${this.agent} 失败写操作去重命中`, { tool: toolCall.name, path: toolCall.arguments?.path }, LogCategory.TOOLS);
        results.push(this.createSyntheticToolResult(
          toolCall,
          failedWriteDedup,
          'error',
          toolSourceMap,
        ));
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
        if (typeof rawResult.content === 'string' && rawResult.content.length > maxToolResultChars) {
          const truncated = rawResult.content.slice(0, maxToolResultChars);
          rawResult.content = `${truncated}\n...[truncated ${rawResult.content.length - maxToolResultChars} chars]`;
        }
        const result = this.ensureStandardizedToolResult(toolCall, rawResult, toolSourceMap);
        results.push(result);

        // 缓存只读工具的成功结果
        if (!result.isError && this.isReadOnlyToolCall(toolCall)) {
          this.cacheSearchResult(toolCall, result.content);
          // 记录文件访问（用于文件级去重）
          this.recordFileAccess(toolCall);
        }

        // 写操作结果追踪：成功则记录到成功缓存并清除失败缓存，失败则记录
        if (!this.isReadOnlyToolCall(toolCall)) {
          if (result.isError) {
            this.recordFailedWrite(toolCall, typeof result.content === 'string' ? result.content : 'Unknown error');
          } else {
            this.clearFailedWriteForPath(toolCall);
            this.recordSuccessWrite(toolCall);
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
    this.conversationHistory = sanitizeToolOrder(this.conversationHistory);
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

    // file_view 是只读工具
    if (name === 'file_view') {
      return true;
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
   * 检索去重：检查工具调用是否与之前的查询高度相似
   *
   * 通过提取查询中的关键标识符（英文单词、文件路径、数字），
   * 计算与缓存查询的 Jaccard 相似度。相似度 > 50% 视为重复。
   *
   * 返回去重提示（命中时）或 null（未命中时）。
   */
  private checkSearchDuplicate(toolCall: ToolCall): string | null {
    if (!this.isReadOnlyToolCall(toolCall)) return null;

    // file_view 是文件读取操作（无论是否带 view_range），
    // 由 L1/L1+ 文件级去重处理，不参与 Jaccard 检索去重
    if (toolCall.name === 'file_view') {
      return null;
    }

    const newIdentifiers = this.extractQueryIdentifiers(toolCall);
    if (newIdentifiers.length === 0) return null;

    const newSet = new Set(newIdentifiers);

    for (const [cachedKey, cachedResult] of this.searchResultCache) {
      const cachedTokens = cachedKey.split('\x00');
      const cachedSet = new Set(cachedTokens);

      // Jaccard 相似度
      const intersection = [...newSet].filter(t => cachedSet.has(t)).length;
      const union = new Set([...newSet, ...cachedSet]).size;
      const similarity = intersection / union;

      if (similarity >= 0.4) {
        this.totalDedupHits++;
        this.roundDedupHits++;

        if (this.totalDedupHits >= 3) {
          // 第3次及以后：不再提供缓存内容，直接拒绝——迫使模型使用已有信息
          return `[系统拒绝] 重复检索已被拦截（第 ${this.totalDedupHits} 次）。你之前已获取过此信息，系统不再重复提供。请立即使用已有信息完成任务，不要继续搜索或查看。`;
        }

        // 前2次：返回缓存结果 + 去重提示
        return `[系统提示] 你之前已执行过相同或高度相似的检索，以下是之前的检索结果（无需再次搜索，请直接使用这些信息继续任务）：\n\n${cachedResult}`;
      }
    }

    return null;
  }

  /**
   * 文件级去重：检查工具调用是否正在访问已完整读取过的文件
   *
   * 解决的核心问题：模型把工具当"数据库查询"用，对同一文件反复发起不同的
   * search/view 请求，而不是一次读取完整文件后从上下文中提取信息。
   * Jaccard 去重无法捕捉这种"不同查询、同一目标文件"的模式。
   *
   * 仅在文件已被完整读取（无 view_range）后才拦截，避免阻止合理的分段读取。
   */
  private checkFileAccessDuplicate(toolCall: ToolCall): string | null {
    if (!this.isReadOnlyToolCall(toolCall)) return null;

    const filePath = this.extractTargetFilePath(toolCall);
    if (!filePath) return null;

    if (this.viewedFiles.has(filePath)) {
      this.totalDedupHits++;
      this.roundDedupHits++;
      const basename = filePath.split('/').pop() || filePath;
      return `[系统提示] 文件 ${basename} 已被完整读取过，全部内容已在你的对话上下文中。请直接从上下文中查找所需信息，不要重复访问同一文件。`;
    }

    // L1+: 分段读取追踪（仅 file_view + view_range）
    if (toolCall.name === 'file_view'
      && toolCall.arguments?.view_range) {
      const range = toolCall.arguments.view_range;
      const rangeKey = Array.isArray(range) ? `${range[0]}-${range[1]}` : String(range);
      const fileRanges = this.viewedRanges.get(filePath);

      if (fileRanges) {
        // 精确去重：同一 range 已读取过
        if (fileRanges.has(rangeKey)) {
          this.totalDedupHits++;
          this.roundDedupHits++;
          const basename = filePath.split('/').pop() || filePath;
          return `[系统提示] 文件 ${basename} 的 ${rangeKey} 行已被读取过，请使用已有内容。`;
        }
        // 碎片化预警：同一文件 ≥3 段分段读取 → 建议全量读取
        if (fileRanges.size >= 3) {
          this.totalDedupHits++;
          this.roundDedupHits++;
          const basename = filePath.split('/').pop() || filePath;
          return `[系统提示] 你已对 ${basename} 进行了 ${fileRanges.size} 次分段读取。请直接使用 file_view（不带 view_range）完整读取该文件，而不是反复分段读取。`;
        }
      }
    }

    return null;
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
    if (this.isReadOnlyToolCall(toolCall)) return null;

    const key = this.buildWriteOperationKey(toolCall);
    const cached = this.failedWriteCache.get(key);
    if (!cached) return null;

    // 相同写操作已失败过 → 短路拦截
    cached.count++;
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
    if (this.isReadOnlyToolCall(toolCall)) return null;

    const key = this.buildContentAwareWriteKey(toolCall);
    if (!this.successWriteCache.has(key)) return null;

    this.totalDedupHits++;
    this.roundDedupHits++;
    this.roundWriteInterceptCount++;

    return `[系统拦截] 此写操作已成功执行，结果已在上下文中。请勿重复相同操作，继续推进任务的下一步。`;
  }

  /**
   * 记录成功的写操作（工具执行成功后调用）
   */
  private recordSuccessWrite(toolCall: ToolCall): void {
    if (this.isReadOnlyToolCall(toolCall)) return;
    const key = this.buildContentAwareWriteKey(toolCall);
    this.successWriteCache.add(key);
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
    if (this.isReadOnlyToolCall(toolCall)) return;
    if (this.shouldSkipFailedWriteCache(error)) return;

    const key = this.buildWriteOperationKey(toolCall);
    const existing = this.failedWriteCache.get(key);
    if (existing) {
      existing.count++;
      existing.error = error;
    } else {
      this.failedWriteCache.set(key, { count: 1, error });
    }
  }

  private shouldSkipFailedWriteCache(error: string): boolean {
    return error.includes('[FILE_CONTEXT_STALE]');
  }

  /**
   * 清除写操作失败缓存（写操作成功时调用，表明状态已变化）
   */
  private clearFailedWriteForPath(toolCall: ToolCall): void {
    // 代码文件发生成功写入后，清空终端命令失败缓存，
    // 避免“修复后重新执行同一构建命令”被历史失败误拦截。
    if (this.isFileMutationTool(toolCall.name)) {
      for (const key of this.failedWriteCache.keys()) {
        if (key.startsWith('launch-process:')) {
          this.failedWriteCache.delete(key);
        }
      }
    }

    // 任何写操作成功后，清除同文件的失败缓存（文件状态已变化，之前的失败可能不再适用）
    const filePath = (toolCall.arguments?.path || toolCall.arguments?.file_path || '') as string;
    if (!filePath) return;
    for (const key of this.failedWriteCache.keys()) {
      if (key.includes(filePath)) {
        this.failedWriteCache.delete(key);
      }
    }
  }

  /**
   * 构建写操作的去重 key（工具名 + 关键参数）
   */
  private buildWriteOperationKey(toolCall: ToolCall): string {
    const args = toolCall.arguments || {};
    if (toolCall.name === 'launch-process') {
      return `launch-process:${String(args.command || '').trim()}:${String(args.cwd || '').trim()}`;
    }
    if (toolCall.name === 'read-process' || toolCall.name === 'write-process' || toolCall.name === 'kill-process') {
      return `${toolCall.name}:${String(args.terminal_id || '')}`;
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

  private isFileMutationTool(toolName: string): boolean {
    return toolName === 'file_edit'
      || toolName === 'file_create'
      || toolName === 'file_insert'
      || toolName === 'file_remove';
  }

  /**
   * 记录文件访问（工具执行成功后调用）
   * 仅在完整读取（无 view_range）时标记为已读，分段读取不标记
   */
  private recordFileAccess(toolCall: ToolCall): void {
    const filePath = this.extractTargetFilePath(toolCall);
    if (!filePath) return;

    if (toolCall.name === 'file_view') {
      if (toolCall.arguments?.view_range) {
        // 分段读取 → 记录 range（用于 L1+ 精确去重和碎片化预警）
        const range = toolCall.arguments.view_range;
        const rangeKey = Array.isArray(range) ? `${range[0]}-${range[1]}` : String(range);
        if (!this.viewedRanges.has(filePath)) {
          this.viewedRanges.set(filePath, new Set());
        }
        this.viewedRanges.get(filePath)!.add(rangeKey);
      } else {
        // 完整读取 → 标记文件
        this.viewedFiles.add(filePath);
      }
    }
    // grep_search、search_context 返回的是片段，不标记为完整读取
  }

  /**
   * 从工具调用中提取目标文件路径（仅返回具体文件，不返回目录）
   */
  private extractTargetFilePath(toolCall: ToolCall): string | null {
    const args = toolCall.arguments || {};

    // file_view → path 字段
    if (toolCall.name === 'file_view') {
      return typeof args.path === 'string' ? args.path : null;
    }

    // grep_search → path 字段（仅当指向具体文件而非目录时）
    if (toolCall.name === 'grep_search' && typeof args.path === 'string') {
      const p = args.path;
      // 有文件扩展名 → 具体文件；无扩展名或以 / 结尾 → 目录，不拦截
      if (/\.\w+$/.test(p)) return p;
    }

    return null;
  }

  /**
   * 缓存只读工具的查询结果
   */
  private cacheSearchResult(toolCall: ToolCall, result: string): void {
    // file_view 是文件读取，不缓存到检索结果中（避免污染 Jaccard 缓存）
    if (toolCall.name === 'file_view') return;

    const identifiers = this.extractQueryIdentifiers(toolCall);
    if (identifiers.length === 0) return;

    // 用 \x00 分隔标识符作为缓存 key
    const key = identifiers.join('\x00');
    this.searchResultCache.set(key, result);
  }

  /**
   * 从工具调用参数中提取关键标识符（英文单词、文件路径片段、数字）
   *
   * 例如查询 "请详细列出与 isWeekend 测试相关的符号：tests/date-utils.test.ts"
   * → ["isWeekend", "tests", "date-utils.test.ts"]
   *
   * 这些标识符用于相似度比较，忽略中文措辞差异，聚焦于实际搜索的代码实体。
   */
  private extractQueryIdentifiers(toolCall: ToolCall): string[] {
    const args = toolCall.arguments || {};
    // 收集所有可能包含搜索意图的字段
    const texts: string[] = [];
    for (const val of Object.values(args)) {
      if (typeof val === 'string') texts.push(val);
    }
    if (texts.length === 0) return [];

    const combined = texts.join(' ');
    // 提取英文标识符（含点号/连字符，捕获文件路径如 date-utils.test.ts）
    // 过滤单字符噪声（如 "d", "s"），避免降低 Jaccard 相似度
    const matches = combined.match(/[a-zA-Z_][\w.-]*/g) || [];
    const filtered = matches.filter(m => m.length >= 2);
    // 去重 + 排序，确保相同标识符集合产生相同 key
    return [...new Set(filtered)].sort();
  }
}
