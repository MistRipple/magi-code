// 渲染辅助函数模块
// 提供所有渲染器共用的工具函数

import { escapeHtml, formatRelativeTime } from '../../core/utils.js';

// ============================================
// 角色和图标
// ============================================

// 获取角色图标
export function getRoleIcon(role) {
  const icons = {
    orchestrator: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 0a8 8 0 1 0 0 16A8 8 0 0 0 8 0zM2.04 4.326c.325 1.329 2.532 2.54 3.717 3.19.48.263.793.434.743.484-.08.08-.162.158-.242.234-.416.396-.787.749-.758 1.266.035.634.618.824 1.214 1.017.577.188 1.168.38 1.286.983.082.417-.075.988-.22 1.52-.215.782-.406 1.48.22 1.48 1.5-.5 3.798-3.186 4-5 .138-1.243-2-2-3.5-2.5-.478-.16-.755.081-.99.284-.172.15-.322.279-.51.216-.445-.148-2.5-2-1.5-2.5.78-.39.952-.171 1.227.182.078.099.163.208.273.318.609.304.662-.132.723-.633.039-.322.081-.671.277-.867.434-.434 1.265-.791 2.028-1.12.712-.306 1.365-.587 1.579-.88A7 7 0 1 1 2.04 4.327z"/></svg>',
    claude: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zM0 8a8 8 0 1 1 16 0A8 8 0 0 1 0 8z"/><path d="M5.255 5.786a.237.237 0 0 0 .241.247h.825c.138 0 .248-.113.266-.25.09-.656.54-1.134 1.342-1.134.686 0 1.314.343 1.314 1.168 0 .635-.374.927-.965 1.371-.673.489-1.206 1.06-1.168 1.987l.003.217a.25.25 0 0 0 .25.246h.811a.25.25 0 0 0 .25-.25v-.105c0-.718.273-.927 1.01-1.486.609-.463 1.244-.977 1.244-2.056 0-1.511-1.276-2.241-2.673-2.241-1.267 0-2.655.59-2.75 2.286zm1.557 5.763c0 .533.425.927 1.01.927.609 0 1.028-.394 1.028-.927 0-.552-.42-.94-1.029-.94-.584 0-1.009.388-1.009.94z"/></svg>',
    codex: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M5.854 4.854a.5.5 0 1 0-.708-.708l-3.5 3.5a.5.5 0 0 0 0 .708l3.5 3.5a.5.5 0 0 0 .708-.708L2.707 8l3.147-3.146zm4.292 0a.5.5 0 0 1 .708-.708l3.5 3.5a.5.5 0 0 1 0 .708l-3.5 3.5a.5.5 0 0 1-.708-.708L13.293 8l-3.147-3.146z"/></svg>',
    gemini: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 0 16A8 8 0 0 1 8 0zM4.5 7.5a.5.5 0 0 0 0 1h5.793l-2.147 2.146a.5.5 0 0 0 .708.708l3-3a.5.5 0 0 0 0-.708l-3-3a.5.5 0 1 0-.708.708L10.293 7.5H4.5z"/></svg>',
    user: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M11 6a3 3 0 1 1-6 0 3 3 0 0 1 6 0z"/><path fill-rule="evenodd" d="M0 8a8 8 0 1 1 16 0A8 8 0 0 1 0 8zm8-7a7 7 0 0 0-5.468 11.37C3.242 11.226 4.805 10 8 10s4.757 1.225 5.468 2.37A7 7 0 0 0 8 1z"/></svg>',
    system: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M9.405 1.05c-.413-1.4-2.397-1.4-2.81 0l-.1.34a1.464 1.464 0 0 1-2.105.872l-.31-.17c-1.283-.698-2.686.705-1.987 1.987l.169.311c.446.82.023 1.841-.872 2.105l-.34.1c-1.4.413-1.4 2.397 0 2.81l.34.1a1.464 1.464 0 0 1 .872 2.105l-.17.31c-.698 1.283.705 2.686 1.987 1.987l.311-.169a1.464 1.464 0 0 1 2.105.872l.1.34c.413 1.4 2.397 1.4 2.81 0l.1-.34a1.464 1.464 0 0 1 2.105-.872l.31.17c1.283.698 2.686-.705 1.987-1.987l-.169-.311a1.464 1.464 0 0 1 .872-2.105l.34-.1c1.4-.413 1.4-2.397 0-2.81l-.34-.1a1.464 1.464 0 0 1-.872-2.105l.17-.31c.698-1.283-.705-2.686-1.987-1.987l-.311.169a1.464 1.464 0 0 1-2.105-.872l-.1-.34zM8 10.93a2.929 2.929 0 1 1 0-5.86 2.929 2.929 0 0 1 0 5.858z"/></svg>',
    info: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 16A8 8 0 1 0 8 0a8 8 0 0 0 0 16zm.93-9.412-1 4.705c-.07.34.029.533.304.533.194 0 .487-.07.686-.246l-.088.416c-.287.346-.92.598-1.465.598-.703 0-1.002-.422-.808-1.319l.738-3.468c.064-.293.006-.399-.287-.47l-.451-.081.082-.381 2.29-.287zM8 5.5a1 1 0 1 1 0-2 1 1 0 0 1 0 2z"/></svg>'
  };
  return icons[role] || icons.info;
}

// 获取角色信息
export function getRoleInfo(message, source, defaultAgent) {
  if (message.role === 'user') {
    return { roleName: '', badgeClass: '' };
  }
  if (source === 'orchestrator') {
    return { roleName: 'Orchestrator', badgeClass: 'orchestrator' };
  }
  const agent = message.agent || defaultAgent;
  if (agent) {
    const agentUpper = agent.toUpperCase();
    return { roleName: agentUpper, badgeClass: agent.toLowerCase() };
  }
  return { roleName: 'AI', badgeClass: 'assistant' };
}

// 获取工具图标
export function getToolIcon(toolName) {
  const name = (toolName || '').toLowerCase();
  if (name.includes('read') || name.includes('view') || name.includes('cat')) {
    return '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor"><path d="M14 4.5V14a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V2a2 2 0 0 1 2-2h5.5L14 4.5zM10 4a1 1 0 0 1-1-1V1.5L13.5 6H11a1 1 0 0 1-1-1V4z"/></svg>';
  }
  if (name.includes('write') || name.includes('edit') || name.includes('save') || name.includes('create') || name.includes('patch')) {
    return '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor"><path d="M12.146.146a.5.5 0 0 1 .708 0l3 3a.5.5 0 0 1 0 .708l-10 10a.5.5 0 0 1-.168.11l-5 2a.5.5 0 0 1-.65-.65l2-5a.5.5 0 0 1 .11-.168l10-10z"/></svg>';
  }
  if (name.includes('search') || name.includes('find') || name.includes('grep')) {
    return '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor"><path d="M11.742 10.344a6.5 6.5 0 1 0-1.397 1.398h-.001c.03.04.062.078.098.115l3.85 3.85a1 1 0 0 0 1.415-1.414l-3.85-3.85a1.007 1.007 0 0 0-.115-.1zM12 6.5a5.5 5.5 0 1 1-11 0 5.5 5.5 0 0 1 11 0z"/></svg>';
  }
  if (name.includes('bash') || name.includes('shell') || name.includes('exec') || name.includes('run') || name.includes('command')) {
    return '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor"><path d="M6 9a.5.5 0 0 1 .5-.5h3a.5.5 0 0 1 0 1h-3A.5.5 0 0 1 6 9zM3.854 4.146a.5.5 0 1 0-.708.708L4.793 6.5 3.146 8.146a.5.5 0 1 0 .708.708l2-2a.5.5 0 0 0 0-.708l-2-2z"/><path d="M2 1a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V3a2 2 0 0 0-2-2H2zm12 1a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H2a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1h12z"/></svg>';
  }
  if (name.includes('list') || name.includes('ls') || name.includes('dir')) {
    return '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor"><path fill-rule="evenodd" d="M5 11.5a.5.5 0 0 1 .5-.5h9a.5.5 0 0 1 0 1h-9a.5.5 0 0 1-.5-.5zm0-4a.5.5 0 0 1 .5-.5h9a.5.5 0 0 1 0 1h-9a.5.5 0 0 1-.5-.5zm0-4a.5.5 0 0 1 .5-.5h9a.5.5 0 0 1 0 1h-9a.5.5 0 0 1-.5-.5zm-3 1a1 1 0 1 0 0-2 1 1 0 0 0 0 2zm0 4a1 1 0 1 0 0-2 1 1 0 0 0 0 2zm0 4a1 1 0 1 0 0-2 1 1 0 0 0 0 2z"/></svg>';
  }
  return '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor"><path d="M1 0L0 1l2.2 3.081a1 1 0 0 0 .815.419h.07a1 1 0 0 1 .708.293l2.675 2.675-2.617 2.654A3.003 3.003 0 0 0 0 13a3 3 0 1 0 5.878-.851l2.654-2.617.968.968-.305.914a1 1 0 0 0 .242 1.023l3.356 3.356a1 1 0 0 0 1.414 0l1.586-1.586a1 1 0 0 0 0-1.414l-3.356-3.356a1 1 0 0 0-1.023-.242l-.914.305-.968-.968 2.617-2.654A3.003 3.003 0 0 0 13 0a3 3 0 1 0-.851 5.878L9.495 8.53 6.82 5.854a1 1 0 0 1-.293-.708v-.07a1 1 0 0 0-.419-.815L1 0zm9.5 5.5a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3zm-9 8a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3z"/></svg>';
}

// ============================================
// 消息分组
// ============================================

// 获取消息分组键
export function getMessageGroupKey(message, source) {
  if (message.role === 'user') return 'user';
  const typeKey = message.messageType || message.type || '';
  return source + '-' + (message.agent || 'ai') + '-' + typeKey;
}

// ============================================
// 内容处理
// ============================================

// 清理内部协议数据
export function cleanInternalProtocolData(content) {
  if (!content) return '';
  return content.replace(/\[INTERNAL:.*?\]/g, '').trim();
}

// 格式化时间
export function formatTime(date) {
  if (!date) return '';
  const d = date instanceof Date ? date : new Date(date);
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

// ============================================
// 内容块提取
// ============================================

// 从内容块中提取文本
export function extractTextFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'text')
    .map(b => b.content)
    .join('\n');
}

// 从内容块中提取代码块
export function extractCodeBlocksFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'code')
    .map(b => ({
      language: b.language || 'text',
      content: b.content,
      filepath: b.filepath
    }));
}

// 从内容块中提取思考内容
export function extractThinkingFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'thinking')
    .map(b => ({
      content: b.content,
      summary: b.summary
    }));
}

// 从内容块中提取工具调用
export function extractToolCallsFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'tool_call')
    .map(b => ({
      id: b.toolId,
      name: b.toolName,
      input: b.input,
      output: b.output,
      result: b.result,
      error: b.error,
      duration: b.duration
    }));
}

// 导出 escapeHtml 供其他模块使用
export { escapeHtml, formatRelativeTime };

