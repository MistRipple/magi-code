/**
 * Claude Skills 工具管理器
 *
 * 管理 Claude 的内置工具（Server-side tools）和自定义工具（Client-side tools）
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
  customTools: ToolDefinition[];
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

  constructor(config?: Partial<SkillsConfig>) {
    this.config = {
      ...DEFAULT_SKILLS_CONFIG,
      ...config,
      builtInTools: {
        ...DEFAULT_SKILLS_CONFIG.builtInTools,
        ...config?.builtInTools
      }
    };

    logger.info('SkillsManager initialized', {
      enabledBuiltInTools: this.getEnabledBuiltInTools().length,
      customTools: this.config.customTools.length
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
      tools.push({
        ...customTool,
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
    tools.push(...this.config.customTools);

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
   * 获取工具定义
   */
  getToolDefinition(toolName: string): ToolDefinition | undefined {
    // 检查内置工具
    const builtInTool = BUILT_IN_TOOL_DEFINITIONS[toolName as BuiltInTool];
    if (builtInTool) {
      return builtInTool;
    }

    // 检查自定义工具
    return this.config.customTools.find(t => t.name === toolName);
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
          // 自定义工具需要外部实现
          throw new Error(`Custom tool execution not implemented: ${name}`);
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
   * 执行文本编辑器工具
   */
  private async executeTextEditor(toolUseId: string, input: any): Promise<SkillToolResult> {
    // TODO: 实现文本编辑器功能
    // 这需要与 VS Code 的文件系统 API 集成
    return {
      type: 'tool_result',
      tool_use_id: toolUseId,
      content: 'Text editor tool not yet implemented',
      is_error: true
    };
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
