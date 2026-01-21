/**
 * Worker LLM 适配器
 * 用于 Worker 代理（claude, codex, gemini）
 */

import { AgentType, AgentRole, LLMConfig, WorkerSlot } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage, ToolCall } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';
import { AgentProfileLoader } from '../../orchestrator/profile/agent-profile-loader';
import { GuidanceInjector } from '../../orchestrator/profile/guidance-injector';

/**
 * Worker 适配器配置
 */
export interface WorkerAdapterConfig {
  client: LLMClient;
  normalizer: BaseNormalizer;
  toolManager: ToolManager;
  config: LLMConfig;
  workerSlot: WorkerSlot;
  systemPrompt?: string;
  profileLoader?: AgentProfileLoader;  // ✅ 新增：用于加载 Agent 画像
}

/**
 * Worker LLM 适配器
 */
export class WorkerLLMAdapter extends BaseLLMAdapter {
  private workerSlot: WorkerSlot;
  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private profileLoader?: AgentProfileLoader;
  private guidanceInjector: GuidanceInjector;

  constructor(adapterConfig: WorkerAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config
    );
    this.workerSlot = adapterConfig.workerSlot;
    this.profileLoader = adapterConfig.profileLoader;
    this.guidanceInjector = new GuidanceInjector();
    this.systemPrompt = adapterConfig.systemPrompt || this.buildSystemPrompt();
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
    if (!this.isConnected) {
      throw new Error('Adapter not connected');
    }

    this.setState(AdapterState.BUSY);
    this.currentTraceId = this.generateTraceId();

    try {
      // 添加用户消息到历史
      this.conversationHistory.push({
        role: 'user',
        content: message,
      });

      // 获取工具定义
      const tools = await this.toolManager.getTools();
      const toolDefinitions = tools.map((tool) => ({
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
      }));

      // 构建请求参数
      const params: LLMMessageParams = {
        messages: this.conversationHistory,
        systemPrompt: this.systemPrompt,
        tools: toolDefinitions.length > 0 ? toolDefinitions : undefined,
        stream: true,
        maxTokens: 4096,
        temperature: 0.7,
      };

      // 开始流式响应
      const messageId = this.normalizer.startStream(this.currentTraceId);
      let fullResponse = '';
      let toolCalls: ToolCall[] = [];

      // 流式调用 LLM
      const response = await this.client.streamMessage(params, (chunk) => {
        if (chunk.type === 'content_delta' && chunk.content) {
          fullResponse += chunk.content;
          this.normalizer.processChunk(messageId, chunk.content);
          this.emit('message', chunk.content);
        } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
          this.emit('toolCall', chunk.toolCall.name || '', chunk.toolCall.arguments || {});
        }
      });

      // 处理工具调用
      if (response.toolCalls && response.toolCalls.length > 0) {
        toolCalls = response.toolCalls;

        // 添加助手响应到历史（包含工具调用）
        const assistantContent: any[] = [];

        // 如果有文本内容，添加文本块
        if (response.content) {
          assistantContent.push({
            type: 'text',
            text: response.content
          });
        }

        // 添加工具使用块
        for (const toolCall of toolCalls) {
          assistantContent.push({
            type: 'tool_use',
            id: toolCall.id,
            name: toolCall.name,
            input: toolCall.arguments
          });
        }

        this.conversationHistory.push({
          role: 'assistant',
          content: assistantContent,
        });

        // 执行工具调用
        const toolResults = await this.executeToolCalls(toolCalls);

        // 添加工具结果到历史（使用 ContentBlock 格式）
        const toolResultContent: any[] = toolResults.map(result => ({
          type: 'tool_result',
          tool_use_id: result.toolCallId,
          content: result.content,
          is_error: result.isError
        }));

        this.conversationHistory.push({
          role: 'user',
          content: toolResultContent,
        });

        // 递归调用以获取最终响应
        this.normalizer.endStream(messageId);
        return await this.sendMessage('Continue based on the tool results.');
      }

      // 添加助手响应到历史
      this.conversationHistory.push({
        role: 'assistant',
        content: fullResponse,
      });

      this.normalizer.endStream(messageId);
      this.setState(AdapterState.CONNECTED);

      return fullResponse;
    } catch (error: any) {
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

  /**
   * 执行工具调用
   */
  private async executeToolCalls(toolCalls: ToolCall[]) {
    const results = [];

    for (const toolCall of toolCalls) {
      try {
        logger.debug(`Executing tool: ${toolCall.name}`, { args: toolCall.arguments }, LogCategory.TOOLS);

        const result = await this.toolManager.execute(toolCall);
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
  private buildSystemPrompt(): string {
    if (!this.profileLoader) {
      return this.getDefaultSystemPrompt();
    }

    try {
      // 加载 Agent 画像
      const agentProfile = this.profileLoader.loadAgentProfile(this.workerSlot);

      // 如果有 guidance，使用 GuidanceInjector 构建
      if (agentProfile.guidance) {
        const workerProfile = this.profileLoader.getProfileLoader().getProfile(this.workerSlot);

        // 构建基础引导 Prompt
        const guidancePrompt = this.guidanceInjector.buildWorkerPrompt(workerProfile, {
          taskDescription: '', // 将在实际任务中填充
        });

        return guidancePrompt;
      }

      return this.getDefaultSystemPrompt();
    } catch (error: any) {
      logger.warn(`Failed to build system prompt from profile: ${error.message}`, undefined, LogCategory.LLM);
      return this.getDefaultSystemPrompt();
    }
  }

  /**
   * 获取默认系统提示
   */
  private getDefaultSystemPrompt(): string {
    return `You are a helpful AI assistant specialized in software development.
You have access to various tools to help complete tasks.
Always think step by step and use tools when appropriate.`;
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
}
