/**
 * 环境上下文提供者
 *
 * 统一管理所有 Agent（Orchestrator 和 Worker）的运行时环境信息。
 *
 * 核心原则 - 单一真相来源：
 * - **工具信息**：从 ToolManager.getTools() 获取（包括内置、MCP、自定义工具）
 * - **提示词/指令**：从 ToolManager.getPrompts() 获取（MCP Prompts + Instruction Skills）
 * - **用户规则**：从 LLMConfigLoader 获取（配置层面的规则）
 *
 * 架构位置：
 * ```
 * LLMConfigLoader.loadSkillsConfig()  ← 配置文件（唯一读取入口）
 *          ↓
 *     SkillsManager / MCPManager（能力持有者）
 *          ↓
 *     ToolManager（统一注册中心）
 *      ├── getTools() → 所有可执行工具
 *      └── getPrompts() → 所有提示词/指令（MCP Prompts + Instruction Skills）
 *          ↓
 *     EnvironmentContextProvider（本类）
 *      └── 从 ToolManager 获取所有信息（不直接读取配置）
 * ```
 */

import { logger, LogCategory } from '../logging';
import { LLMConfigLoader } from '../llm/config';
import type { ToolManager, UnifiedPromptInfo } from '../tools/tool-manager';
import type { ExtendedToolDefinition } from '../tools/types';

/**
 * 环境上下文提供者配置
 */
export interface EnvironmentContextConfig {
  /** 工作区路径 */
  workspace: string;
  /** 终端当前目录 */
  terminalCwd?: string;
}

/**
 * 环境上下文提供者
 *
 * 使用方式：
 * ```ts
 * const provider = new EnvironmentContextProvider({ workspace: '/path' });
 * provider.setToolManager(toolManager);
 * await provider.refresh();
 * const prompt = provider.getEnvironmentPrompt();
 * ```
 */
export class EnvironmentContextProvider {
  private config: EnvironmentContextConfig;
  private toolManager: ToolManager | null = null;

  // 缓存（由 refresh() 更新）
  private cachedTools: ExtendedToolDefinition[] = [];
  private cachedPrompts: UnifiedPromptInfo[] = [];

  constructor(config: EnvironmentContextConfig) {
    this.config = config;
  }

  /**
   * 注入 ToolManager（唯一数据来源）
   */
  setToolManager(manager: ToolManager | null): void {
    this.toolManager = manager;
    this.cachedTools = [];
    this.cachedPrompts = [];
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<EnvironmentContextConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /**
   * 刷新缓存（从 ToolManager 获取最新数据）
   * 应在以下时机调用：
   * - 初始化后
   * - MCP/Skills 重新加载后
   * - 创建新 Adapter 前
   */
  async refresh(): Promise<void> {
    if (!this.toolManager) {
      logger.warn('EnvironmentContextProvider: ToolManager 未设置', undefined, LogCategory.TOOLS);
      return;
    }

    try {
      // 1. 获取所有工具（通过 ToolManager 单一入口）
      this.cachedTools = await this.toolManager.getTools();

      // 2. 获取所有 Prompts（通过 ToolManager 单一入口）
      this.cachedPrompts = this.toolManager.getPrompts();

      logger.debug('EnvironmentContextProvider.refresh', {
        tools: this.cachedTools.length,
        prompts: this.cachedPrompts.length,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.warn('刷新环境上下文缓存失败', { error: error.message }, LogCategory.TOOLS);
    }
  }

  /**
   * 获取完整的环境提示（用于注入到系统提示）
   */
  getEnvironmentPrompt(): string {
    const sections: string[] = [];

    // 1. IDE 状态
    sections.push(this.getIDEStatePrompt());

    // 2. 可用工具
    const toolsPrompt = this.getToolsPrompt();
    if (toolsPrompt) {
      sections.push(toolsPrompt);
    }

    // 3. 提示词/指令（MCP Prompts + Instruction Skills）
    const promptsSection = this.getPromptsPrompt();
    if (promptsSection) {
      sections.push(promptsSection);
    }

    // 4. 用户规则
    const rulesPrompt = this.getUserRulesPrompt();
    if (rulesPrompt) {
      sections.push(rulesPrompt);
    }

    return sections.filter(Boolean).join('\n\n');
  }

  /**
   * 获取 IDE 状态提示
   */
  getIDEStatePrompt(): string {
    const { workspace, terminalCwd } = this.config;
    return `## IDE 状态
- **工作区**: ${workspace}
- **终端目录**: ${terminalCwd || workspace}
- **时间**: ${new Date().toISOString()}`;
  }

  /**
   * 获取工具列表提示（从缓存）
   */
  getToolsPrompt(): string {
    if (this.cachedTools.length === 0) {
      return '';
    }

    const blocks: string[] = ['## 可用工具'];

    // 按来源分组
    const builtinTools = this.cachedTools.filter(t => t.metadata?.source === 'builtin');
    const mcpTools = this.cachedTools.filter(t => t.metadata?.source === 'mcp');
    const skillTools = this.cachedTools.filter(t => t.metadata?.source === 'skill');

    // 内置工具（简要列出）
    if (builtinTools.length > 0) {
      blocks.push('\n### 内置工具');
      blocks.push(`可用: ${builtinTools.map(t => t.name).join(', ')}`);
    }

    // MCP 工具（按服务器分组）
    if (mcpTools.length > 0) {
      blocks.push('\n### MCP 工具');
      const byServer = new Map<string, ExtendedToolDefinition[]>();
      for (const tool of mcpTools) {
        const serverId = tool.metadata?.sourceId || 'unknown';
        if (!byServer.has(serverId)) {
          byServer.set(serverId, []);
        }
        byServer.get(serverId)!.push(tool);
      }
      for (const [serverId, tools] of byServer) {
        blocks.push(`\n**${serverId}**:`);
        for (const tool of tools) {
          const desc = tool.description ? ` - ${tool.description}` : '';
          blocks.push(`- ${tool.name}${desc}`);
        }
      }
    }

    // Skill 自定义工具
    if (skillTools.length > 0) {
      blocks.push('\n### 自定义工具');
      for (const tool of skillTools) {
        const desc = tool.description ? ` - ${tool.description}` : '';
        blocks.push(`- ${tool.name}${desc}`);
      }
    }

    logger.debug('环境上下文.工具', {
      builtin: builtinTools.length,
      mcp: mcpTools.length,
      skill: skillTools.length,
    }, LogCategory.TOOLS);

    return blocks.join('\n');
  }

  /**
   * 获取提示词/指令提示（从缓存）
   * 统一展示 MCP Prompts 和 Instruction Skills
   */
  getPromptsPrompt(): string {
    if (this.cachedPrompts.length === 0) {
      return '';
    }

    const mcpPrompts = this.cachedPrompts.filter(p => p.source === 'mcp');
    const skillPrompts = this.cachedPrompts.filter(p => p.source === 'skill');

    const blocks: string[] = ['## 可用 Skills / Prompts'];
    blocks.push('- 你可以在合适的任务中主动使用这些能力。');
    blocks.push('- 当用户输入 `/skill-name` 时，必须应用对应指令。');
    blocks.push('');

    // MCP Prompts
    if (mcpPrompts.length > 0) {
      blocks.push('### MCP Prompts');
      for (const prompt of mcpPrompts) {
        const desc = prompt.description ? ` - ${prompt.description}` : '';
        blocks.push(`- **${prompt.name}**${desc}`);
        if (prompt.arguments && prompt.arguments.length > 0) {
          const args = prompt.arguments.map(a => `${a.name}${a.required ? '*' : ''}`).join(', ');
          blocks.push(`  参数: ${args}`);
        }
      }
      blocks.push('');
    }

    // Instruction Skills
    if (skillPrompts.length > 0) {
      const autoSkills = skillPrompts.filter(s => !s.disableModelInvocation);
      const manualSkills = skillPrompts.filter(s => s.disableModelInvocation);

      // Skill 列表摘要
      blocks.push('### Skill 列表');
      for (const skill of skillPrompts) {
        const flag = skill.disableModelInvocation ? '（仅手动 /skill）' : '';
        blocks.push(`- **${skill.name}**${flag}: ${skill.description || ''}`);
      }
      blocks.push('');

      // 自动调用 Skill 的详细指令
      if (autoSkills.length > 0) {
        blocks.push('### Skill 指令（可自动调用）');
        const maxChars = 6000;
        let usedChars = 0;

        for (const skill of autoSkills) {
          const contentBlock = this.formatSkillInstruction(skill);
          if (usedChars + contentBlock.length > maxChars) {
            blocks.push(`- **${skill.name}**: 指令内容过长，需在 /${skill.name} 调用时加载`);
            continue;
          }
          blocks.push(contentBlock);
          usedChars += contentBlock.length;
        }
        blocks.push('');
      }

      // 仅手动调用的 Skills
      if (manualSkills.length > 0) {
        blocks.push('### 仅在 /skill 调用时启用的 Skills');
        for (const skill of manualSkills) {
          blocks.push(`- **${skill.name}**: ${skill.description || ''}`);
        }
      }
    }

    logger.debug('环境上下文.Prompts', {
      mcp: mcpPrompts.length,
      skill: skillPrompts.length,
    }, LogCategory.TOOLS);

    return blocks.join('\n');
  }

  /**
   * 格式化单个 Skill 指令
   */
  private formatSkillInstruction(skill: UnifiedPromptInfo): string {
    const toolHint = Array.isArray(skill.allowedTools) && skill.allowedTools.length > 0
      ? `允许使用的工具: ${skill.allowedTools.join(', ')}`
      : '';
    const argHint = skill.argumentHint ? `参数提示: ${skill.argumentHint}` : '';
    const hints = [toolHint, argHint].filter(Boolean).join(' | ');

    return [
      `\n**[${skill.name}]**`,
      skill.description ? `描述: ${skill.description}` : '',
      hints ? `提示: ${hints}` : '',
      skill.content ? `指令:\n${skill.content}` : '',
    ].filter(Boolean).join('\n');
  }

  /**
   * 获取用户规则提示
   */
  getUserRulesPrompt(): string {
    try {
      const rules = LLMConfigLoader.loadUserRules();
      if (!rules.enabled || !rules.content?.trim()) {
        return '';
      }

      return `<!-- USER_RULES_START -->
## 用户规则
${rules.content.trim()}
<!-- USER_RULES_END -->`;
    } catch (error: any) {
      logger.warn('获取用户规则失败', { error: error.message }, LogCategory.LLM);
      return '';
    }
  }
}

