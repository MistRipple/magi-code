/**
 * Skills Manager (精简版)
 *
 * 仅管理指令型 Skills 和自定义工具
 * 内置工具（launch-process/read-process/write-process/kill-process/list-processes, text_editor, grep_search 等）由 ToolManager 管理
 */

import { logger, LogCategory } from '../logging/unified-logger';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult as LLMToolResult } from '../llm/types';

/**
 * 工具定义接口
 */
export interface ToolDefinition {
  name: string;
  description: string;
  input_schema: {
    type: 'object';
    properties: Record<string, any>;
    required?: string[];
  };
}

/**
 * 自定义工具执行器配置
 */
export interface CustomToolExecutorConfig {
  type: 'static' | 'template' | 'http';
  response?: string;
  template?: string;
  url?: string;
  method?: string;
  headers?: Record<string, string>;
  bodyTemplate?: string;
  timeoutMs?: number;
}

/**
 * 自定义工具定义
 */
export interface CustomToolDefinition extends ToolDefinition {
  executor?: CustomToolExecutorConfig;
  repositoryId?: string;
  repositoryName?: string;
}

/**
 * 指令型 Skill（来自 SKILL.md）
 */
export interface InstructionSkillDefinition {
  name: string;
  description: string;
  content: string;
  allowedTools?: string[];
  disableModelInvocation?: boolean;
  userInvocable?: boolean;
  argumentHint?: string;
  repositoryId?: string;
  repositoryName?: string;
}

/**
 * Skills 配置
 */
export interface SkillsConfig {
  customTools: CustomToolDefinition[];
  instructionSkills: InstructionSkillDefinition[];
}

/**
 * Skills Manager
 *
 * 管理指令型 Skills 和自定义工具
 * 内置工具由 ToolManager 管理
 */
export class SkillsManager implements ToolExecutor {
  private config: SkillsConfig;

  constructor(config?: Partial<SkillsConfig>) {
    this.config = {
      customTools: config?.customTools || [],
      instructionSkills: config?.instructionSkills || [],
    };

    logger.info('SkillsManager initialized', {
      customTools: this.config.customTools.length,
      instructionSkills: this.config.instructionSkills.length,
    }, LogCategory.TOOLS);
  }

  /**
   * 实现 ToolExecutor 接口：执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<LLMToolResult> {
    logger.info('Executing custom tool', { name: toolCall.name, id: toolCall.id }, LogCategory.TOOLS);

    try {
      const result = await this.executeCustomTool(toolCall.id, toolCall.name, toolCall.arguments);
      return {
        toolCallId: toolCall.id,
        content: result.content,
        isError: result.is_error || false,
      };
    } catch (error: any) {
      logger.error('Custom tool execution failed', {
        name: toolCall.name,
        error: error.message,
      }, LogCategory.TOOLS);

      return {
        toolCallId: toolCall.id,
        content: `Error: ${error.message}`,
        isError: true,
      };
    }
  }

  /**
   * 实现 ToolExecutor 接口：获取工具定义列表
   */
  async getTools(): Promise<ExtendedToolDefinition[]> {
    const tools: ExtendedToolDefinition[] = [];

    // 添加自定义工具
    for (const customTool of this.config.customTools) {
      tools.push({
        name: customTool.name,
        description: customTool.description,
        input_schema: customTool.input_schema,
        metadata: {
          source: 'skill',
          sourceId: customTool.name,
          category: 'custom',
          tags: ['custom'],
        },
      });
    }

    return tools;
  }

  /**
   * 实现 ToolExecutor 接口：检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    return this.config.customTools.some(t => t.name === toolName);
  }

  // ============================================================================
  // 自定义工具管理
  // ============================================================================

  /**
   * 添加自定义工具
   */
  addCustomTool(tool: CustomToolDefinition): void {
    const existingIndex = this.config.customTools.findIndex(t => t.name === tool.name);
    if (existingIndex >= 0) {
      this.config.customTools[existingIndex] = tool;
      logger.info('Custom tool updated', { name: tool.name }, LogCategory.TOOLS);
    } else {
      this.config.customTools.push(tool);
      logger.info('Custom tool added', { name: tool.name }, LogCategory.TOOLS);
    }
  }

  /**
   * 删除自定义工具
   */
  removeCustomTool(toolName: string): void {
    const index = this.config.customTools.findIndex(t => t.name === toolName);
    if (index >= 0) {
      this.config.customTools.splice(index, 1);
      logger.info('Custom tool removed', { name: toolName }, LogCategory.TOOLS);
    }
  }

  /**
   * 获取自定义工具定义
   */
  getCustomToolDefinition(toolName: string): CustomToolDefinition | undefined {
    return this.config.customTools.find(t => t.name === toolName);
  }

  /**
   * 获取所有自定义工具
   */
  getCustomTools(): CustomToolDefinition[] {
    return [...this.config.customTools];
  }

  /**
   * 执行自定义工具
   */
  private async executeCustomTool(
    toolUseId: string,
    name: string,
    input: any
  ): Promise<{ content: string; is_error?: boolean }> {
    const customTool = this.config.customTools.find(tool => tool.name === name);
    if (!customTool) {
      return {
        content: `Error: custom tool not found: ${name}`,
        is_error: true,
      };
    }

    if (!customTool.executor) {
      return {
        content: `Error: custom tool '${name}' has no executor configured`,
        is_error: true,
      };
    }

    switch (customTool.executor.type) {
      case 'static':
        return { content: customTool.executor.response ?? '' };

      case 'template': {
        const template = customTool.executor.template ?? '';
        const rendered = this.renderTemplate(template, input);
        return { content: rendered };
      }

      case 'http':
        return await this.executeHttpTool(customTool.executor, input);

      default:
        return {
          content: `Error: unsupported executor type ${(customTool.executor as any).type}`,
          is_error: true,
        };
    }
  }

  private renderTemplate(template: string, input: Record<string, any>): string {
    return template.replace(/{{\s*([\w.-]+)\s*}}/g, (_match, pathKey) => {
      const value = this.getValueByPath(input, pathKey);
      if (value === undefined || value === null) {
        return '';
      }
      if (typeof value === 'string') {
        return value;
      }
      return JSON.stringify(value);
    });
  }

  private getValueByPath(source: Record<string, any>, pathKey: string): any {
    return pathKey.split('.').reduce((acc: any, key: string) => {
      if (acc && typeof acc === 'object' && key in acc) {
        return acc[key];
      }
      return undefined;
    }, source);
  }

  private async executeHttpTool(
    executor: CustomToolExecutorConfig,
    input: Record<string, any>
  ): Promise<{ content: string; is_error?: boolean }> {
    if (!executor.url) {
      return { content: 'Error: http executor requires url', is_error: true };
    }

    const method = (executor.method || 'POST').toUpperCase();
    const headers = executor.headers ? { ...executor.headers } : {};
    const timeoutMs = executor.timeoutMs ?? 15000;
    const controller = new AbortController();
    const timeoutHandle = setTimeout(() => controller.abort(), timeoutMs);

    try {
      let url = executor.url;
      let body: string | undefined;

      if (method === 'GET') {
        const query = new URLSearchParams();
        Object.entries(input || {}).forEach(([key, value]) => {
          if (value === undefined) return;
          query.append(key, typeof value === 'string' ? value : JSON.stringify(value));
        });
        if (query.toString()) {
          url += (url.includes('?') ? '&' : '?') + query.toString();
        }
      } else {
        if (executor.bodyTemplate) {
          body = this.renderTemplate(executor.bodyTemplate, input || {});
        } else {
          body = JSON.stringify(input || {});
          if (!headers['Content-Type']) {
            headers['Content-Type'] = 'application/json';
          }
        }
      }

      const response = await fetch(url, {
        method,
        headers,
        body,
        signal: controller.signal,
      });

      const contentType = response.headers.get('content-type') || '';
      let content: string;

      if (contentType.includes('application/json')) {
        const data = await response.json();
        content = JSON.stringify(data, null, 2);
      } else {
        content = await response.text();
      }

      return { content, is_error: !response.ok };
    } catch (error: any) {
      return { content: `Error: ${error.message}`, is_error: true };
    } finally {
      clearTimeout(timeoutHandle);
    }
  }

  // ============================================================================
  // 指令型 Skills 管理
  // ============================================================================

  /**
   * 添加指令型 Skill
   */
  addInstructionSkill(skill: InstructionSkillDefinition): void {
    const existingIndex = this.config.instructionSkills.findIndex(s => s.name === skill.name);
    if (existingIndex >= 0) {
      this.config.instructionSkills[existingIndex] = skill;
      logger.info('Instruction skill updated', { name: skill.name }, LogCategory.TOOLS);
    } else {
      this.config.instructionSkills.push(skill);
      logger.info('Instruction skill added', { name: skill.name }, LogCategory.TOOLS);
    }
  }

  /**
   * 删除指令型 Skill
   */
  removeInstructionSkill(skillName: string): void {
    const index = this.config.instructionSkills.findIndex(s => s.name === skillName);
    if (index >= 0) {
      this.config.instructionSkills.splice(index, 1);
      logger.info('Instruction skill removed', { name: skillName }, LogCategory.TOOLS);
    }
  }

  /**
   * 获取指令型 Skill
   */
  getInstructionSkill(skillName: string): InstructionSkillDefinition | undefined {
    return this.config.instructionSkills.find(s => s.name === skillName);
  }

  /**
   * 获取所有指令型 Skills
   */
  getInstructionSkills(): InstructionSkillDefinition[] {
    return [...this.config.instructionSkills];
  }

  /**
   * 获取用户可调用的指令型 Skills
   */
  getUserInvocableSkills(): InstructionSkillDefinition[] {
    return this.config.instructionSkills.filter(s => s.userInvocable);
  }

  // ============================================================================
  // 配置管理
  // ============================================================================

  /**
   * 获取配置
   */
  getConfig(): SkillsConfig {
    return { ...this.config };
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<SkillsConfig>): void {
    if (config.customTools) {
      this.config.customTools = config.customTools;
    }
    if (config.instructionSkills) {
      this.config.instructionSkills = config.instructionSkills;
    }

    logger.info('Skills config updated', {
      customTools: this.config.customTools.length,
      instructionSkills: this.config.instructionSkills.length,
    }, LogCategory.TOOLS);
  }
}
