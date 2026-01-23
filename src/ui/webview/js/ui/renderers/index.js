// 渲染器模块统一导出
// 提供所有渲染相关功能的统一入口

// 辅助函数
export {
  getRoleIcon,
  getRoleInfo,
  getToolIcon,
  getMessageGroupKey,
  cleanInternalProtocolData,
  formatTime,
  extractTextFromBlocks,
  extractCodeBlocksFromBlocks,
  extractThinkingFromBlocks,
  extractToolCallsFromBlocks,
  escapeHtml,
  formatRelativeTime
} from './render-utils.js';

// Markdown 渲染
export {
  renderMarkdown,
  renderCodeBlock,
  renderParsedBlocks,
  renderThinkingBlock,
  renderToolUseBlock
} from './markdown-renderer.js';

// 卡片渲染
export {
  renderUnifiedCard,
  renderToolCallItem,
  renderToolTrack,
  renderTaskCard
} from './card-renderer.js';

