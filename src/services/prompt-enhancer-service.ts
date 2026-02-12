/**
 * PromptEnhancerService - 提示词增强服务
 *
 * 从 WebviewProvider 提取的业务逻辑（P1-1 修复）。
 * 职责：收集代码上下文 + 调用 LLM 增强用户 prompt。
 */

import fs from 'fs';
import path from 'path';
import { logger, LogCategory } from '../logging';
import type { ToolManager } from '../tools/tool-manager';

/**
 * PromptEnhancerService 依赖接口
 * 通过依赖注入避免直接引用 WebviewProvider 的内部状态
 */
export interface PromptEnhancerDeps {
  workspaceRoot: string;
  getToolManager: () => ToolManager | undefined;
  getConversationHistory: (maxRounds: number) => string;
}

export interface EnhanceResult {
  enhancedPrompt: string;
  error?: string;
}

export class PromptEnhancerService {
  constructor(private deps: PromptEnhancerDeps) {}

  /**
   * 增强用户 prompt
   * 收集代码上下文 + 对话历史，调用 LLM 生成增强后的 prompt
   */
  async enhance(prompt: string): Promise<EnhanceResult> {
    try {
      const { LLMConfigLoader } = await import('../llm/config');
      const compressorConfig = LLMConfigLoader.loadCompressorConfig();
      const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();

      const useCompressor = compressorConfig.enabled
        && Boolean(compressorConfig.baseUrl && compressorConfig.model);
      const activeConfig = useCompressor ? compressorConfig : orchestratorConfig;
      const activeLabel = useCompressor ? 'compressor' : 'orchestrator';

      // 收集代码上下文
      let codeContext = '';
      if (this.deps.workspaceRoot) {
        try {
          codeContext = await this.collectCodeContext(this.deps.workspaceRoot, prompt);
        } catch (error) {
          logger.warn('提示词增强.代码上下文收集失败', { error }, LogCategory.UI);
        }
      }

      // 收集对话历史
      const conversationHistory = this.deps.getConversationHistory(10);

      // 检测语言
      const isChinese = /[\u4e00-\u9fa5]/.test(prompt);

      // 构建增强 prompt
      const enhancePrompt = this.buildEnhancePrompt(prompt, conversationHistory, codeContext, isChinese);

      // 创建 LLM 客户端并调用
      const { UniversalLLMClient } = await import('../llm/clients/universal-client');
      const client = new UniversalLLMClient({
        baseUrl: activeConfig.baseUrl,
        apiKey: activeConfig.apiKey,
        model: activeConfig.model,
        provider: activeConfig.provider,
        enabled: true,
      });

      logger.info('提示词增强.开始', {
        model: activeConfig.model,
        used: activeLabel,
        fallbackToOrchestrator: !useCompressor,
        hasCodeContext: codeContext.length > 0,
        hasConversation: conversationHistory.length > 0,
      }, LogCategory.UI);

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: enhancePrompt }],
        maxTokens: 4096,
        temperature: 0.7,
      });

      const enhancedPrompt = response.content?.trim() || '';

      if (enhancedPrompt) {
        logger.info('提示词增强.完成', {
          originalLength: prompt.length,
          enhancedLength: enhancedPrompt.length,
        }, LogCategory.UI);
        return { enhancedPrompt };
      }

      return { enhancedPrompt: '', error: '未获取到增强结果' };
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      logger.error('提示词增强.失败', { error: errorMsg }, LogCategory.UI);
      return { enhancedPrompt: '', error: errorMsg };
    }
  }

  // ============================================================================
  // 代码上下文收集
  // ============================================================================

  /**
   * 收集代码上下文
   * 统一通过 AceExecutor 获取（ACE 可用时语义搜索，不可用时自动降级到本地三级搜索）
   */
  private async collectCodeContext(projectRoot: string, prompt: string): Promise<string> {
    const contextParts: string[] = [];
    const maxContextLength = 8000;
    let currentLength = 0;

    try {
      // 1. 通过 AceExecutor 统一获取代码上下文（自动降级）
      const toolManager = this.deps.getToolManager();
      if (toolManager) {
        const aceExecutor = toolManager.getAceExecutor();
        const toolCall = {
          id: `enhance-ace-${Date.now()}`,
          name: 'codebase_retrieval',
          arguments: {
            query: prompt,
            ensure_indexed: false,
          },
        };

        const result = await aceExecutor.execute(toolCall);
        if (!result.isError && result.content && result.content !== '未找到相关代码（本地三级搜索无结果）') {
          contextParts.push(`## 相关代码\n${result.content}`);
          currentLength += result.content.length;
          logger.info('提示词增强.代码上下文获取成功', { resultLength: result.content.length }, LogCategory.UI);
        }
      }

      // 2. 检查项目说明文件
      const guidelineFiles = ['CLAUDE.md', '.augment-guidelines', 'README.md', 'CONTRIBUTING.md'];
      for (const guideFile of guidelineFiles) {
        if (currentLength >= maxContextLength) break;

        const guidePath = path.join(projectRoot, guideFile);
        if (fs.existsSync(guidePath)) {
          try {
            const content = fs.readFileSync(guidePath, 'utf-8');
            const truncatedContent = content.length > 3000
              ? content.substring(0, 3000) + '\n... (truncated)'
              : content;

            if (currentLength + truncatedContent.length <= maxContextLength) {
              contextParts.push(`## 项目指南 (${guideFile})\n${truncatedContent}`);
              currentLength += truncatedContent.length;
              break;
            }
          } catch (error) {
            logger.debug('提示词增强.指南文件读取失败', { file: guideFile, error }, LogCategory.UI);
          }
        }
      }
    } catch (error) {
      logger.warn('提示词增强.代码上下文收集异常', { error }, LogCategory.UI);
    }

    return contextParts.join('\n\n');
  }

  // ============================================================================
  // 辅助方法
  // ============================================================================

  extractKeywords(prompt: string): string[] {
    const words = prompt.split(/[\s,，。.!！?？;；:：()（）[\]【】{}]+/);
    const keywords: string[] = [];

    for (const word of words) {
      const cleaned = word.trim();
      if (cleaned.length < 2) continue;
      if (cleaned.length > 50) continue;

      if (/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(cleaned)) {
        keywords.push(cleaned);
      }
      if (/[\u4e00-\u9fa5]{2,}/.test(cleaned)) {
        keywords.push(cleaned);
      }
      if (/\.[a-z]{1,5}$/i.test(cleaned)) {
        keywords.push(cleaned);
      }
    }

    return [...new Set(keywords)].slice(0, 10);
  }

  private buildEnhancePrompt(
    originalPrompt: string,
    conversationHistory: string,
    codeContext: string,
    isChinese: boolean,
  ): string {
    const languageInstruction = isChinese
      ? '请用中文输出增强后的提示词。'
      : 'Please output the enhanced prompt in English.';

    return `You are an expert prompt engineer. Your task is to enhance the user's original prompt to make it clearer, more specific, and more actionable for an AI coding assistant.

## Enhancement Principles

1. **Clarify Intent**: Make the task goal crystal clear
2. **Add Technical Context**: Include relevant technical details, constraints, and requirements
3. **Structure the Request**: Organize the prompt with clear sections if needed
4. **Make it Actionable**: Ensure the AI can directly execute the task
5. **Preserve User Intent**: Do not change the user's original intention
6. **Use Code Context**: Reference relevant files, functions, or patterns from the codebase when applicable
7. **Consider Existing Patterns**: Align suggestions with existing code patterns and conventions

${codeContext ? `## Codebase Context

The following is relevant context from the user's project:

${codeContext}

` : ''}## Conversation History

${conversationHistory ? conversationHistory : '(No previous conversation)'}

## Original Prompt

${originalPrompt}

## Output Requirements

- ${languageInstruction}
- Output ONLY the enhanced prompt, without any explanations or prefixes
- Do NOT include prefixes like "Enhanced prompt:" or "增强后的提示词："
- Keep it concise but complete
- If the original prompt references code or files, make sure to maintain those references
- Add specific technical details that would help the AI assistant complete the task`;
  }
}
