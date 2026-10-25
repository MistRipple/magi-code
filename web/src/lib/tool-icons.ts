import type { IconName } from './icons';
import { parseToolIdentity } from './tool-identity';

/** 工具图标的唯一展示映射；正式会话与设计预览共用。 */
export function resolveToolIcon(toolName: string): IconName {
  const parsedTool = parseToolIdentity(toolName);
  if (parsedTool.source === 'mcp') {
    return 'plug';
  }
  if (parsedTool.source === 'skill') {
    return 'plug';
  }
  const baseToolName = parsedTool.baseName;

  const iconMap: Record<string, IconName> = {
    'view_image': 'eye',
    'image_generate': 'sparkles',
    'file_remove': 'trash',
    'web_search': 'search',
    'web_fetch': 'globe',
    'diagram_render': 'git-branch',
    'skill_apply': 'skill',
    'shell_exec': 'terminal',
    'file_read': 'eye',
    'file_write': 'file-plus',
    'file_patch': 'pencil',
    'apply_patch': 'file-edit',
    'file_mkdir': 'folder',
    'file_copy': 'file-plus',
    'file_move': 'file-plus',
    'search_text': 'search',
    'search_semantic': 'search',
    'process_inspect': 'terminal',
    'diff_preview': 'file-text',
    'knowledge_query': 'question',
    'code_symbols': 'search',
    'tool_catalog': 'tool',
    'agent_spawn': 'bot',
    'agent_wait': 'hourglass',
    'agent_apply': 'git-branch',
    'update_plan': 'list',
    'memory_write': 'database',
    'ask_user_question': 'question',
  };

  if (iconMap[baseToolName]) {
    return iconMap[baseToolName];
  }

  const lowerName = baseToolName.toLowerCase();
  if (lowerName.includes('search') || lowerName.includes('semantic') || lowerName.includes('query')) return 'search';
  if (lowerName.includes('read') || lowerName.includes('view')) return 'file-text';
  if (lowerName.includes('write') || lowerName.includes('edit')) return 'file-edit';
  if (lowerName.includes('delete') || lowerName.includes('remove')) return 'file-minus';
  if (lowerName.includes('process')) return 'terminal';
  if (lowerName.includes('web') || lowerName.includes('fetch') || lowerName.includes('browser')) return 'globe';
  if (lowerName.includes('diagram') || lowerName.includes('mermaid')) return 'git-branch';
  if (lowerName.includes('task')) return 'document';
  if (lowerName.includes('mcp')) return 'plug';
  return 'tool';
}
