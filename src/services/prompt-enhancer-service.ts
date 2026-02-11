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
import type { ProjectKnowledgeBase } from '../knowledge/project-knowledge-base';

/**
 * PromptEnhancerService 依赖接口
 * 通过依赖注入避免直接引用 WebviewProvider 的内部状态
 */
export interface PromptEnhancerDeps {
  workspaceRoot: string;
  getToolManager: () => ToolManager | undefined;
  getKnowledgeBase: () => ProjectKnowledgeBase | undefined;
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
   * 优先使用 ACE 语义搜索，回退到本地多策略搜索
   */
  private async collectCodeContext(projectRoot: string, prompt: string): Promise<string> {
    const contextParts: string[] = [];
    const maxContextLength = 8000;
    let currentLength = 0;

    try {
      // 1. 尝试 ACE 语义搜索
      const aceResult = await this.tryAceSemanticSearch(projectRoot, prompt);
      if (aceResult) {
        contextParts.push(`## 相关代码（语义搜索）\n${aceResult}`);
        currentLength += aceResult.length;
        logger.info('提示词增强.ACE语义搜索成功', { resultLength: aceResult.length }, LogCategory.UI);
      } else {
        // 2. ACE 不可用，使用本地多策略搜索
        logger.info('提示词增强.ACE不可用，使用本地多策略搜索', undefined, LogCategory.UI);
        const localResult = await this.performLocalContextSearch(projectRoot, prompt, maxContextLength);
        if (localResult) {
          contextParts.push(localResult);
          currentLength += localResult.length;
        }
      }

      // 3. 检查项目说明文件
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

  /**
   * 使用 ACE 语义搜索获取代码上下文
   */
  private async tryAceSemanticSearch(_projectRoot: string, prompt: string): Promise<string | null> {
    try {
      const toolManager = this.deps.getToolManager();
      if (!toolManager) return null;
      if (!toolManager.isAceConfigured()) return null;

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
      if (!result.isError && result.content && result.content !== '未找到相关代码') {
        return result.content;
      }

      return null;
    } catch (error) {
      logger.warn('提示词增强.ACE搜索异常', { error }, LogCategory.UI);
      return null;
    }
  }

  /**
   * 本地多策略上下文搜索（grep + LSP + 知识库索引）
   */
  private async performLocalContextSearch(
    projectRoot: string,
    prompt: string,
    maxContextLength: number,
  ): Promise<string | null> {
    const toolManager = this.deps.getToolManager();
    if (!toolManager) return null;

    const parts: string[] = [];
    let currentLength = 0;

    // 1. 提取搜索关键词
    const keywords = this.extractKeywords(prompt);
    if (keywords.length === 0) {
      const structure = await this.getProjectStructure(projectRoot);
      return structure ? `## 项目结构\n${structure}` : null;
    }

    // 2. 知识库项目上下文
    const knowledgeBase = this.deps.getKnowledgeBase();
    if (knowledgeBase) {
      const projectContext = knowledgeBase.getProjectContext(400);
      if (projectContext) {
        parts.push(`## 项目概览\n${projectContext}`);
        currentLength += projectContext.length;
      }

      // 倒排索引 + TF-IDF
      try {
        const searchResults = await knowledgeBase.search(prompt, {
          maxResults: 8,
          maxContextLength: Math.floor((maxContextLength - currentLength) * 0.6),
        });
        if (searchResults.length > 0) {
          const snippetText = searchResults
            .map(r => `### ${r.filePath} (得分: ${r.score.toFixed(2)})\n` +
              r.snippets.map(s => `\`\`\`\n${s.content}\n\`\`\``).join('\n'))
            .join('\n\n');
          parts.push(`## 相关代码（索引检索）\n${snippetText}`);
          currentLength += snippetText.length;
          logger.info('提示词增强.本地索引搜索命中', {
            hits: searchResults.length,
            length: snippetText.length,
          }, LogCategory.UI);
        }
      } catch (error) {
        logger.warn('提示词增强.本地索引搜索失败', { error }, LogCategory.UI);
      }
    }

    // 3. grep 搜索
    const grepResults = await this.grepSearchForContext(toolManager, keywords, maxContextLength - currentLength);
    if (grepResults) {
      parts.push(`## 相关代码（关键词匹配）\n${grepResults}`);
      currentLength += grepResults.length;
    }

    // 4. LSP 符号搜索
    if (currentLength < maxContextLength * 0.8) {
      const symbolResults = await this.lspSymbolSearchForContext(
        toolManager, keywords, maxContextLength - currentLength,
      );
      if (symbolResults) {
        parts.push(`## 相关符号定义\n${symbolResults}`);
        currentLength += symbolResults.length;
      }
    }

    if (parts.length === 0) {
      const structure = await this.getProjectStructure(projectRoot);
      return structure ? `## 项目结构\n${structure}` : null;
    }

    logger.info('提示词增强.本地搜索完成', {
      strategies: parts.length,
      totalLength: currentLength,
      keywords: keywords.slice(0, 3),
    }, LogCategory.UI);

    return parts.join('\n\n');
  }

  // ============================================================================
  // 搜索策略（公开方法，供 ACE 本地搜索回退复用）
  // ============================================================================

  async grepSearchForContext(
    toolManager: ToolManager,
    keywords: string[],
    maxLength: number,
  ): Promise<string | null> {
    try {
      const searchExecutor = toolManager.getSearchExecutor();
      if (!searchExecutor) return null;

      const results: string[] = [];
      let totalLength = 0;
      const searchKeywords = keywords.filter(kw => kw.length >= 3).slice(0, 3);

      for (const keyword of searchKeywords) {
        if (totalLength >= maxLength) break;

        try {
          const escapedKeyword = keyword.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
          const toolCall = {
            id: `ctx-grep-${Date.now()}-${keyword}`,
            name: 'grep_search',
            arguments: {
              pattern: escapedKeyword,
              include: '*.ts,*.tsx,*.js,*.jsx,*.py,*.go,*.rs,*.java,*.c,*.cpp,*.h,*.hpp,*.cs,*.php,*.rb,*.swift,*.kt,*.m,*.vue',
              context_lines: 2,
              case_sensitive: false,
            },
          };

          const result = await searchExecutor.execute(toolCall);
          if (!result.isError && result.content && result.content !== 'No matches found') {
            const maxPerKeyword = Math.floor(maxLength / searchKeywords.length);
            const truncated = result.content.length > maxPerKeyword
              ? result.content.substring(0, maxPerKeyword) + '\n... (更多结果已省略)'
              : result.content;

            results.push(`### 关键词: "${keyword}"\n${truncated}`);
            totalLength += truncated.length;
          }
        } catch (error) {
          logger.debug('提示词增强.grep单关键词搜索失败', { keyword, error }, LogCategory.UI);
        }
      }

      return results.length > 0 ? results.join('\n\n') : null;
    } catch (error) {
      logger.warn('提示词增强.grep搜索失败', { error }, LogCategory.UI);
      return null;
    }
  }

  async lspSymbolSearchForContext(
    toolManager: ToolManager,
    keywords: string[],
    maxLength: number,
  ): Promise<string | null> {
    try {
      const lspExecutor = toolManager.getLspExecutor();
      if (!lspExecutor) return null;

      const symbolEntries: string[] = [];
      let totalLength = 0;
      const symbolKeywords = keywords
        .filter(kw => /^[a-zA-Z_][a-zA-Z0-9_]*$/.test(kw) && kw.length >= 3)
        .slice(0, 3);

      for (const keyword of symbolKeywords) {
        if (totalLength >= maxLength) break;

        try {
          const toolCall = {
            id: `ctx-lsp-${Date.now()}-${keyword}`,
            name: 'lsp_query',
            arguments: {
              action: 'workspaceSymbols',
              query: keyword,
            },
          };

          const result = await lspExecutor.execute(toolCall);
          if (!result.isError && result.content) {
            const parsed = JSON.parse(result.content);
            const symbols = parsed.symbols || [];
            if (symbols.length === 0) continue;

            const formatted = symbols.slice(0, 10).map((sym: any) => {
              const loc = sym.location;
              const uri = loc?.uri || '';
              const filePath = uri.replace(/^file:\/\//, '').replace(this.deps.workspaceRoot + '/', '');
              const line = loc?.range?.start?.line ?? '?';
              return `  - ${sym.kindName || 'symbol'} **${sym.name}** → ${filePath}:${line}`;
            }).join('\n');

            const entry = `### "${keyword}" 的符号定义\n${formatted}`;
            if (totalLength + entry.length <= maxLength) {
              symbolEntries.push(entry);
              totalLength += entry.length;
            }
          }
        } catch (error) {
          logger.debug('提示词增强.LSP单符号搜索失败', { keyword, error }, LogCategory.UI);
        }
      }

      return symbolEntries.length > 0 ? symbolEntries.join('\n\n') : null;
    } catch (error) {
      logger.warn('提示词增强.LSP符号搜索失败', { error }, LogCategory.UI);
      return null;
    }
  }

  // ============================================================================
  // 辅助方法
  // ============================================================================

  private async getProjectStructure(projectRoot: string): Promise<string> {
    const structure: string[] = [];
    const maxDepth = 3;
    const maxFiles = 50;
    let fileCount = 0;

    const excludeDirs = new Set([
      'node_modules', '.git', '.vscode', 'dist', 'build', 'out',
      '__pycache__', '.pytest_cache', 'coverage', '.next', '.nuxt',
    ]);

    const walk = (dir: string, depth: number, prefix: string = '') => {
      if (depth > maxDepth || fileCount > maxFiles) return;

      try {
        const items = fs.readdirSync(dir, { withFileTypes: true });
        const sortedItems = items.sort((a, b) => {
          if (a.isDirectory() && !b.isDirectory()) return -1;
          if (!a.isDirectory() && b.isDirectory()) return 1;
          return a.name.localeCompare(b.name);
        });

        for (const item of sortedItems) {
          if (fileCount > maxFiles) break;
          if (item.name.startsWith('.') && item.name !== '.env.example') continue;
          if (excludeDirs.has(item.name)) continue;

          const isLast = items.indexOf(item) === items.length - 1;
          const connector = isLast ? '└── ' : '├── ';
          const newPrefix = isLast ? '    ' : '│   ';

          if (item.isDirectory()) {
            structure.push(`${prefix}${connector}${item.name}/`);
            walk(path.join(dir, item.name), depth + 1, prefix + newPrefix);
          } else {
            structure.push(`${prefix}${connector}${item.name}`);
            fileCount++;
          }
        }
      } catch (error) {
        logger.debug('提示词增强.目录遍历权限问题', { dir, error }, LogCategory.UI);
      }
    };

    walk(projectRoot, 0);

    if (structure.length === 0) return '';
    if (fileCount > maxFiles) {
      structure.push('... (more files)');
    }

    return structure.slice(0, 100).join('\n');
  }

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
