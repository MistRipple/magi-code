/**
 * Claude Skills 工具管理器
 *
 * 管理 Claude 的内置工具（Server-side tools）和自定义工具（Client-side tools）
 */

import { logger, LogCategory } from '../logging/unified-logger';
import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult as LLMToolResult } from '../llm/types';
import * as fs from 'fs/promises';
import * as path from 'path';

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
 * 工具使用请求
 */
export interface ToolUseRequest {
  type: 'tool_use';
  id: string;
  name: string;
  input: Record<string, any>;
}

/**
 * 工具结果（Skills 内部使用）
 */
export interface SkillToolResult {
  type: 'tool_result';
  tool_use_id: string;
  content: string | Array<{ type: string; [key: string]: any }>;
  is_error?: boolean;
}

/**
 * 内置工具类型
 */
export enum BuiltInTool {
  WEB_SEARCH = 'web_search_20250305',
  WEB_FETCH = 'web_fetch_20250305',
  TEXT_EDITOR = 'text_editor_20250124',
  COMPUTER_USE = 'computer_use_20241022'
}

/**
 * 工具配置
 */
export interface ToolConfig {
  enabled: boolean;
  description?: string;
}

/**
 * Skills 配置
 */
export interface SkillsConfig {
  builtInTools: {
    [BuiltInTool.WEB_SEARCH]: ToolConfig;
    [BuiltInTool.WEB_FETCH]: ToolConfig;
    [BuiltInTool.TEXT_EDITOR]: ToolConfig;
    [BuiltInTool.COMPUTER_USE]: ToolConfig;
  };
  customTools: CustomToolDefinition[];
  instructionSkills?: InstructionSkillDefinition[];
}

/**
 * 默认工具配置
 */
const DEFAULT_SKILLS_CONFIG: SkillsConfig = {
  builtInTools: {
    [BuiltInTool.WEB_SEARCH]: {
      enabled: true,
      description: '搜索网络以获取最新信息'
    },
    [BuiltInTool.WEB_FETCH]: {
      enabled: true,
      description: '获取网页内容'
    },
    [BuiltInTool.TEXT_EDITOR]: {
      enabled: false,
      description: '编辑文本文件（需要客户端实现）'
    },
    [BuiltInTool.COMPUTER_USE]: {
      enabled: false,
      description: '控制计算机（需要客户端实现）'
    }
  },
  customTools: []
};

/**
 * 内置工具定义
 */
const BUILT_IN_TOOL_DEFINITIONS: Record<BuiltInTool, ToolDefinition> = {
  [BuiltInTool.WEB_SEARCH]: {
    name: BuiltInTool.WEB_SEARCH,
    description: 'Search the web for information. This is a server-side tool that executes on Anthropic\'s servers.',
    input_schema: {
      type: 'object',
      properties: {
        query: {
          type: 'string',
          description: 'The search query to execute'
        }
      },
      required: ['query']
    }
  },
  [BuiltInTool.WEB_FETCH]: {
    name: BuiltInTool.WEB_FETCH,
    description: 'Fetch and analyze content from a URL. This is a server-side tool that executes on Anthropic\'s servers.',
    input_schema: {
      type: 'object',
      properties: {
        url: {
          type: 'string',
          description: 'The URL to fetch content from'
        },
        prompt: {
          type: 'string',
          description: 'Optional prompt to guide content analysis'
        }
      },
      required: ['url']
    }
  },
  [BuiltInTool.TEXT_EDITOR]: {
    name: BuiltInTool.TEXT_EDITOR,
    description: 'Edit text files using commands like view, create, str_replace, insert, and undo_edit. This is a client-side tool that requires implementation.',
    input_schema: {
      type: 'object',
      properties: {
        command: {
          type: 'string',
          enum: ['view', 'create', 'str_replace', 'insert', 'undo_edit'],
          description: 'The editing command to execute'
        },
        path: {
          type: 'string',
          description: 'The file path to operate on'
        },
        file_text: {
          type: 'string',
          description: 'The content for create command'
        },
        old_str: {
          type: 'string',
          description: 'The string to replace (for str_replace)'
        },
        new_str: {
          type: 'string',
          description: 'The replacement string (for str_replace)'
        },
        insert_line: {
          type: 'number',
          description: 'The line number to insert at (for insert)'
        },
        insert_text: {
          type: 'string',
          description: 'The text to insert (for insert)'
        }
      },
      required: ['command', 'path']
    }
  },
  [BuiltInTool.COMPUTER_USE]: {
    name: BuiltInTool.COMPUTER_USE,
    description: 'Control the computer by taking screenshots, moving the mouse, clicking, typing, and more. This is a client-side tool that requires implementation.',
    input_schema: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: ['key', 'type', 'mouse_move', 'left_click', 'right_click', 'middle_click', 'double_click', 'screenshot', 'cursor_position'],
          description: 'The action to perform'
        },
        text: {
          type: 'string',
          description: 'Text to type (for type action)'
        },
        coordinate: {
          type: 'array',
          items: { type: 'number' },
          description: 'X, Y coordinates (for mouse actions)'
        }
      },
      required: ['action']
    }
  }
};

/**
 * Skills Manager
 *
 * 管理 Claude 的工具系统
 */
export class SkillsManager implements ToolExecutor {
  private config: SkillsConfig;
  private workspaceRoot: string;
  private undoStack: Map<string, string> = new Map();

  constructor(config?: Partial<SkillsConfig>, options?: { workspaceRoot?: string }) {
    this.config = {
      ...DEFAULT_SKILLS_CONFIG,
      ...config,
      builtInTools: {
        ...DEFAULT_SKILLS_CONFIG.builtInTools,
        ...config?.builtInTools
      }
    };
    this.workspaceRoot = options?.workspaceRoot || process.cwd();

    logger.info('SkillsManager initialized', {
      enabledBuiltInTools: this.getEnabledBuiltInTools().length,
      customTools: this.config.customTools.length,
      workspaceRoot: this.workspaceRoot
    }, LogCategory.TOOLS);
  }

  /**
   * 实现 ToolExecutor 接口：执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<LLMToolResult> {
    logger.info('Executing skill tool', { name: toolCall.name, id: toolCall.id }, LogCategory.TOOLS);

    try {
      // 检查是否是服务器端工具（由 Claude API 执行，不需要客户端处理）
      if (this.isServerSideTool(toolCall.name)) {
        return {
          toolCallId: toolCall.id,
          content: 'Server-side tool executed by Claude API',
          isError: false,
        };
      }

      // 执行客户端工具
      const toolUseRequest: ToolUseRequest = {
        type: 'tool_use',
        id: toolCall.id,
        name: toolCall.name,
        input: toolCall.arguments,
      };

      const result = await this.executeClientTool(toolUseRequest);

      return {
        toolCallId: toolCall.id,
        content: typeof result.content === 'string' ? result.content : JSON.stringify(result.content),
        isError: result.is_error || false,
      };
    } catch (error: any) {
      logger.error('Skill tool execution failed', {
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

    // 添加启用的内置工具
    for (const [toolName, toolConfig] of Object.entries(this.config.builtInTools)) {
      if (toolConfig.enabled) {
        const definition = BUILT_IN_TOOL_DEFINITIONS[toolName as BuiltInTool];
        if (definition) {
          tools.push({
            ...definition,
            metadata: {
              source: 'skill',
              sourceId: toolName,
              category: this.isServerSideTool(toolName) ? 'server-side' : 'client-side',
              tags: ['claude', 'builtin'],
            },
          });
        }
      }
    }

    // 添加自定义工具
    for (const customTool of this.config.customTools) {
      const definition = this.stripCustomTool(customTool);
      tools.push({
        ...definition,
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
    // 检查内置工具
    const builtInTool = BUILT_IN_TOOL_DEFINITIONS[toolName as BuiltInTool];
    if (builtInTool) {
      const config = this.config.builtInTools[toolName as BuiltInTool];
      return config?.enabled || false;
    }

    // 检查自定义工具
    return this.config.customTools.some(t => t.name === toolName);
  }

  /**
   * 获取所有启用的工具定义
   */
  getEnabledTools(): ToolDefinition[] {
    const tools: ToolDefinition[] = [];

    // 添加启用的内置工具
    for (const [toolName, toolConfig] of Object.entries(this.config.builtInTools)) {
      if (toolConfig.enabled) {
        const definition = BUILT_IN_TOOL_DEFINITIONS[toolName as BuiltInTool];
        if (definition) {
          tools.push(definition);
        }
      }
    }

    // 添加自定义工具
    tools.push(...this.config.customTools.map(tool => this.stripCustomTool(tool)));

    return tools;
  }

  /**
   * 获取启用的内置工具列表
   */
  getEnabledBuiltInTools(): BuiltInTool[] {
    return Object.entries(this.config.builtInTools)
      .filter(([_, config]) => config.enabled)
      .map(([name, _]) => name as BuiltInTool);
  }

  /**
   * 检查工具是否为服务器端工具
   */
  isServerSideTool(toolName: string): boolean {
    return toolName === BuiltInTool.WEB_SEARCH || toolName === BuiltInTool.WEB_FETCH;
  }

  /**
   * 检查工具是否为客户端工具
   */
  isClientSideTool(toolName: string): boolean {
    return toolName === BuiltInTool.TEXT_EDITOR ||
           toolName === BuiltInTool.COMPUTER_USE ||
           this.config.customTools.some(t => t.name === toolName);
  }

  /**
   * 启用内置工具
   */
  enableBuiltInTool(tool: BuiltInTool): void {
    if (this.config.builtInTools[tool]) {
      this.config.builtInTools[tool].enabled = true;
      logger.info('Built-in tool enabled', { tool }, LogCategory.TOOLS);
    }
  }

  /**
   * 禁用内置工具
   */
  disableBuiltInTool(tool: BuiltInTool): void {
    if (this.config.builtInTools[tool]) {
      this.config.builtInTools[tool].enabled = false;
      logger.info('Built-in tool disabled', { tool }, LogCategory.TOOLS);
    }
  }

  /**
   * 添加自定义工具
   */
  addCustomTool(tool: ToolDefinition): void {
    // 检查是否已存在
    const existingIndex = this.config.customTools.findIndex(t => t.name === tool.name);
    if (existingIndex >= 0) {
      this.config.customTools[existingIndex] = tool as CustomToolDefinition;
      logger.info('Custom tool updated', { name: tool.name }, LogCategory.TOOLS);
    } else {
      this.config.customTools.push(tool as CustomToolDefinition);
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
   * 获取工具定义
   */
  getToolDefinition(toolName: string): ToolDefinition | undefined {
    // 检查内置工具
    const builtInTool = BUILT_IN_TOOL_DEFINITIONS[toolName as BuiltInTool];
    if (builtInTool) {
      return builtInTool;
    }

    // 检查自定义工具
    const customTool = this.config.customTools.find(t => t.name === toolName);
    return customTool ? this.stripCustomTool(customTool) : undefined;
  }

  /**
   * 执行客户端工具
   */
  async executeClientTool(toolUse: ToolUseRequest): Promise<SkillToolResult> {
    const { id, name, input } = toolUse;

    logger.info('Executing client tool', { name, input }, LogCategory.TOOLS);

    try {
      // 根据工具类型执行
      switch (name) {
        case BuiltInTool.TEXT_EDITOR:
          return await this.executeTextEditor(id, input);

        case BuiltInTool.COMPUTER_USE:
          return await this.executeComputerUse(id, input);

        default:
          return await this.executeCustomTool(id, name, input);
      }
    } catch (error: any) {
      logger.error('Client tool execution failed', {
        name,
        error: error.message
      }, LogCategory.TOOLS);

      return {
        type: 'tool_result',
        tool_use_id: id,
        content: `Error: ${error.message}`,
        is_error: true
      };
    }
  }

  /**
   * 执行自定义工具
   */
  private async executeCustomTool(toolUseId: string, name: string, input: any): Promise<SkillToolResult> {
    const customTool = this.config.customTools.find(tool => tool.name === name);
    if (!customTool) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: `Error: custom tool not found: ${name}`,
        is_error: true
      };
    }

    if (!customTool.executor) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: `Error: custom tool '${name}' has no executor configured`,
        is_error: true
      };
    }

    switch (customTool.executor.type) {
      case 'static':
        return {
          type: 'tool_result',
          tool_use_id: toolUseId,
          content: customTool.executor.response ?? ''
        };

      case 'template': {
        const template = customTool.executor.template ?? '';
        const rendered = this.renderTemplate(template, input);
        return {
          type: 'tool_result',
          tool_use_id: toolUseId,
          content: rendered
        };
      }

      case 'http':
        return await this.executeHttpTool(toolUseId, customTool.executor, input);

      default:
        return {
          type: 'tool_result',
          tool_use_id: toolUseId,
          content: `Error: unsupported executor type ${(customTool.executor as any).type}`,
          is_error: true
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
    toolUseId: string,
    executor: CustomToolExecutorConfig,
    input: Record<string, any>
  ): Promise<SkillToolResult> {
    if (!executor.url) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: 'Error: http executor requires url',
        is_error: true
      };
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
        signal: controller.signal
      });

      const contentType = response.headers.get('content-type') || '';
      let content: string;

      if (contentType.includes('application/json')) {
        const data = await response.json();
        content = JSON.stringify(data, null, 2);
      } else {
        content = await response.text();
      }

      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content,
        is_error: !response.ok
      };
    } catch (error: any) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: `Error: ${error.message}`,
        is_error: true
      };
    } finally {
      clearTimeout(timeoutHandle);
    }
  }

  private stripCustomTool(customTool: CustomToolDefinition): ToolDefinition {
    return {
      name: customTool.name,
      description: customTool.description,
      input_schema: customTool.input_schema
    };
  }

  /**
   * 执行文本编辑器工具
   */
  private async executeTextEditor(toolUseId: string, input: any): Promise<SkillToolResult> {
    const command = input?.command;
    const targetPath = input?.path;

    if (!command || !targetPath) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: 'Error: command and path are required',
        is_error: true
      };
    }

    const resolved = this.resolveWorkspacePath(targetPath);
    if (!resolved) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: `Error: path is outside workspace: ${targetPath}`,
        is_error: true
      };
    }

    try {
      switch (command) {
        case 'view': {
          const content = await fs.readFile(resolved, 'utf-8');
          return {
            type: 'tool_result',
            tool_use_id: toolUseId,
            content
          };
        }

        case 'create': {
          const fileText = input?.file_text ?? '';
          await fs.mkdir(path.dirname(resolved), { recursive: true });
          try {
            await fs.access(resolved);
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: 'Error: file already exists',
              is_error: true
            };
          } catch {
            await fs.writeFile(resolved, fileText, 'utf-8');
          }
          return {
            type: 'tool_result',
            tool_use_id: toolUseId,
            content: 'OK: file created'
          };
        }

        case 'str_replace': {
          const oldStr = input?.old_str;
          const newStr = input?.new_str ?? '';
          if (typeof oldStr !== 'string') {
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: 'Error: old_str is required',
              is_error: true
            };
          }
          let content = '';
          try {
            content = await fs.readFile(resolved, 'utf-8');
          } catch (error: any) {
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: `Error: ${error.message}`,
              is_error: true
            };
          }
          const index = content.indexOf(oldStr);
          if (index === -1) {
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: 'Error: old_str not found',
              is_error: true
            };
          }
          this.undoStack.set(resolved, content);
          const updated = content.replace(oldStr, newStr);
          await fs.writeFile(resolved, updated, 'utf-8');
          return {
            type: 'tool_result',
            tool_use_id: toolUseId,
            content: 'OK: str_replace applied'
          };
        }

        case 'insert': {
          const insertLine = Number(input?.insert_line);
          const insertText = input?.insert_text ?? '';
          if (!Number.isFinite(insertLine) || insertLine < 1) {
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: 'Error: insert_line must be a positive number',
              is_error: true
            };
          }
          let content = '';
          try {
            content = await fs.readFile(resolved, 'utf-8');
          } catch (error: any) {
            if (error?.code !== 'ENOENT') {
              return {
                type: 'tool_result',
                tool_use_id: toolUseId,
                content: `Error: ${error.message}`,
                is_error: true
              };
            }
          }
          const lines = content.split('\n');
          if (insertLine > lines.length + 1) {
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: 'Error: insert_line out of range',
              is_error: true
            };
          }
          this.undoStack.set(resolved, content);
          lines.splice(insertLine - 1, 0, insertText);
          await fs.mkdir(path.dirname(resolved), { recursive: true });
          await fs.writeFile(resolved, lines.join('\n'), 'utf-8');
          return {
            type: 'tool_result',
            tool_use_id: toolUseId,
            content: 'OK: text inserted'
          };
        }

        case 'undo_edit': {
          if (!this.undoStack.has(resolved)) {
            return {
              type: 'tool_result',
              tool_use_id: toolUseId,
              content: 'Error: no undo history for this file',
              is_error: true
            };
          }
          const previous = this.undoStack.get(resolved) ?? '';
          await fs.writeFile(resolved, previous, 'utf-8');
          this.undoStack.delete(resolved);
          return {
            type: 'tool_result',
            tool_use_id: toolUseId,
            content: 'OK: undo applied'
          };
        }

        default:
          return {
            type: 'tool_result',
            tool_use_id: toolUseId,
            content: `Error: unsupported command ${command}`,
            is_error: true
          };
      }
    } catch (error: any) {
      return {
        type: 'tool_result',
        tool_use_id: toolUseId,
        content: `Error: ${error.message}`,
        is_error: true
      };
    }
  }

  /**
   * 执行计算机使用工具
   */
  private async executeComputerUse(toolUseId: string, input: any): Promise<SkillToolResult> {
    // TODO: 实现计算机控制功能
    // 这需要系统级权限和额外的安全考虑
    return {
      type: 'tool_result',
      tool_use_id: toolUseId,
      content: 'Computer use tool not yet implemented',
      is_error: true
    };
  }

  private resolveWorkspacePath(inputPath: string): string | null {
    const resolved = path.resolve(this.workspaceRoot, inputPath);
    const normalizedRoot = path.resolve(this.workspaceRoot) + path.sep;
    if (!resolved.startsWith(normalizedRoot)) {
      return null;
    }
    return resolved;
  }

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
    this.config = {
      ...this.config,
      ...config,
      builtInTools: {
        ...this.config.builtInTools,
        ...config.builtInTools
      }
    };

    logger.info('Skills config updated', {
      enabledBuiltInTools: this.getEnabledBuiltInTools().length,
      customTools: this.config.customTools.length
    }, LogCategory.TOOLS);
  }
}
