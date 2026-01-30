/**
 * Mermaid 图表执行器
 * 提供 Mermaid 图表渲染功能
 *
 * 工具: mermaid_diagram
 */

import { ToolExecutor, ExtendedToolDefinition } from './types';
import { ToolCall, ToolResult } from '../llm/types';
import { logger, LogCategory } from '../logging';

/**
 * Mermaid 执行器
 */
export class MermaidExecutor implements ToolExecutor {
  constructor() {
    // Mermaid 执行器不需要工作区路径
  }

  /**
   * 获取工具定义
   */
  getToolDefinition(): ExtendedToolDefinition {
    return {
      name: 'mermaid_diagram',
      description: `Render Mermaid diagrams for visualization.

Use for:
* Flowcharts and process diagrams
* Sequence diagrams
* Class diagrams
* Entity relationship diagrams
* State diagrams
* Gantt charts
* Pie charts

The diagram will be rendered as an interactive SVG with pan/zoom controls.

Example:
\`\`\`mermaid
graph TD
    A[Start] --> B{Decision}
    B -->|Yes| C[Action 1]
    B -->|No| D[Action 2]
\`\`\``,
      input_schema: {
        type: 'object',
        properties: {
          code: {
            type: 'string',
            description: 'Mermaid diagram code to render'
          },
          title: {
            type: 'string',
            description: 'Optional title for the diagram'
          },
          theme: {
            type: 'string',
            enum: ['default', 'dark', 'forest', 'neutral'],
            description: 'Diagram theme (default: uses system theme)'
          }
        },
        required: ['code']
      },
      metadata: {
        source: 'builtin',
        category: 'visualization',
        tags: ['mermaid', 'diagram', 'visualization', 'chart']
      }
    };
  }

  /**
   * 获取所有工具（实现 ToolExecutor 接口）
   */
  async getTools(): Promise<ExtendedToolDefinition[]> {
    return [this.getToolDefinition()];
  }

  /**
   * 检查工具是否可用
   */
  async isAvailable(toolName: string): Promise<boolean> {
    return toolName === 'mermaid_diagram';
  }

  /**
   * 执行工具调用
   */
  async execute(toolCall: ToolCall): Promise<ToolResult> {
    const args = toolCall.arguments as {
      code: string;
      title?: string;
      theme?: 'default' | 'dark' | 'forest' | 'neutral';
    };

    if (!args.code) {
      return {
        toolCallId: toolCall.id,
        content: 'Error: code is required',
        isError: true
      };
    }

    logger.debug('MermaidExecutor executing', { hasTitle: !!args.title }, LogCategory.TOOLS);

    try {
      // 验证 Mermaid 代码基本语法
      const validationResult = this.validateMermaidCode(args.code);
      if (!validationResult.valid) {
        return {
          toolCallId: toolCall.id,
          content: `Error: ${validationResult.error}`,
          isError: true
        };
      }

      // 生成渲染数据
      const renderData = {
        type: 'mermaid_diagram',
        code: args.code.trim(),
        title: args.title,
        theme: args.theme || 'default',
        diagramType: validationResult.diagramType
      };

      logger.info('Mermaid diagram prepared', {
        diagramType: validationResult.diagramType,
        codeLength: args.code.length
      }, LogCategory.TOOLS);

      // 返回渲染数据（由前端 webview 处理渲染）
      return {
        toolCallId: toolCall.id,
        content: JSON.stringify(renderData, null, 2),
        isError: false
      };
    } catch (error: any) {
      logger.error('MermaidExecutor error', { error: error.message }, LogCategory.TOOLS);
      return {
        toolCallId: toolCall.id,
        content: `Error: ${error.message}`,
        isError: true
      };
    }
  }

  /**
   * 验证 Mermaid 代码
   */
  private validateMermaidCode(code: string): { valid: boolean; error?: string; diagramType?: string } {
    const trimmed = code.trim();

    if (!trimmed) {
      return { valid: false, error: 'Mermaid code is empty' };
    }

    // 检测图表类型
    const diagramTypes = [
      { pattern: /^graph\s+/i, type: 'flowchart' },
      { pattern: /^flowchart\s+/i, type: 'flowchart' },
      { pattern: /^sequenceDiagram/i, type: 'sequence' },
      { pattern: /^classDiagram/i, type: 'class' },
      { pattern: /^stateDiagram/i, type: 'state' },
      { pattern: /^erDiagram/i, type: 'er' },
      { pattern: /^gantt/i, type: 'gantt' },
      { pattern: /^pie/i, type: 'pie' },
      { pattern: /^journey/i, type: 'journey' },
      { pattern: /^gitGraph/i, type: 'git' },
      { pattern: /^mindmap/i, type: 'mindmap' },
      { pattern: /^timeline/i, type: 'timeline' },
      { pattern: /^quadrantChart/i, type: 'quadrant' },
      { pattern: /^requirementDiagram/i, type: 'requirement' },
      { pattern: /^C4Context/i, type: 'c4' },
      { pattern: /^sankey/i, type: 'sankey' },
      { pattern: /^xychart/i, type: 'xychart' },
      { pattern: /^block-beta/i, type: 'block' }
    ];

    for (const { pattern, type } of diagramTypes) {
      if (pattern.test(trimmed)) {
        return { valid: true, diagramType: type };
      }
    }

    // 未识别的图表类型
    return {
      valid: false,
      error: 'Unrecognized Mermaid diagram type. Code should start with a valid diagram declaration (e.g., graph, flowchart, sequenceDiagram, classDiagram, etc.)'
    };
  }
}
