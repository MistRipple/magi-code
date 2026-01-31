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
* Mind maps (思维导图)

**IMPORTANT**: Always provide a descriptive 'title' parameter that describes what the diagram represents (e.g., "用户登录流程图", "系统架构图", "订单状态机"). This title will be used as the tab name when opened in VS Code.

The diagram will be rendered as an interactive SVG with pan/zoom controls.

Flowchart Example:
\`\`\`mermaid
graph TD
    A[Start] --> B{Decision}
    B -->|Yes| C[Action 1]
    B -->|No| D[Action 2]
\`\`\`

Mindmap Example (IMPORTANT: use 2-space indentation for hierarchy):
\`\`\`mermaid
mindmap
  root((Central Topic))
    Branch 1
      Sub-item 1.1
      Sub-item 1.2
    Branch 2
      Sub-item 2.1
      Sub-item 2.2
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
            description: 'Descriptive title for the diagram (STRONGLY RECOMMENDED). Used as tab name in VS Code. Example: "用户登录流程图", "系统架构思维导图"'
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
        // mindmap 需要额外验证缩进格式
        if (type === 'mindmap') {
          const mindmapValidation = this.validateMindmapIndentation(trimmed);
          if (!mindmapValidation.valid) {
            return mindmapValidation;
          }
        }
        return { valid: true, diagramType: type };
      }
    }

    // 未识别的图表类型
    return {
      valid: false,
      error: 'Unrecognized Mermaid diagram type. Code should start with a valid diagram declaration (e.g., graph, flowchart, sequenceDiagram, classDiagram, etc.)'
    };
  }

  /**
   * 验证 mindmap 缩进格式
   * Mermaid mindmap 语法要求使用缩进表示层级关系
   */
  private validateMindmapIndentation(code: string): { valid: boolean; error?: string; diagramType?: string } {
    const lines = code.split('\n');

    // 跳过第一行 "mindmap"
    const contentLines = lines.slice(1).filter(line => line.trim() !== '');

    if (contentLines.length === 0) {
      return { valid: false, error: 'Mindmap has no content after declaration' };
    }

    // 检查是否有缩进结构
    let hasIndentedContent = false;
    let rootFound = false;

    for (const line of contentLines) {
      const leadingSpaces = line.match(/^(\s*)/)?.[1]?.length || 0;
      const trimmedLine = line.trim();

      // 检测 root 节点（通常是 root((xxx)) 或 root(xxx) 或 root[xxx]）
      if (trimmedLine.match(/^root[\(\[\{]/i)) {
        rootFound = true;
        continue;
      }

      // 如果 root 已找到，后续行应该有缩进
      if (rootFound && leadingSpaces > 0) {
        hasIndentedContent = true;
        break;
      }
    }

    // 如果找到了 root 但后续内容没有缩进，说明格式错误
    if (rootFound && contentLines.length > 1 && !hasIndentedContent) {
      // 检查是否所有内容行都在同一层级（没有缩进）
      const nonRootLines = contentLines.filter(line => !line.trim().match(/^root[\(\[\{]/i));
      const allNoIndent = nonRootLines.every(line => {
        const spaces = line.match(/^(\s*)/)?.[1]?.length || 0;
        return spaces === 0;
      });

      if (allNoIndent && nonRootLines.length > 0) {
        return {
          valid: false,
          error: `Mindmap syntax error: Missing indentation for hierarchy.
Each child node must be indented with spaces to show its level.

Correct format:
mindmap
  root((Topic))
    Branch 1
      Sub-item 1.1
    Branch 2
      Sub-item 2.1

Your code has all nodes at the same level without indentation.
Please add 2-space indentation for each hierarchy level.`
        };
      }
    }

    return { valid: true, diagramType: 'mindmap' };
  }
}
