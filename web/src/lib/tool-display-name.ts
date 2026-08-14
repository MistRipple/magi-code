import { parseToolIdentity } from './tool-identity';

interface TranslationSource {
  t(key: string, params?: Record<string, string | number>): string;
}

const TOOL_DISPLAY_NAME_KEYS: Record<string, string> = {
  tool_result: 'toolCall.displayName.default',
  skill_apply: 'toolCall.displayName.skillApply',
  shell_exec: 'toolCall.displayName.shell',
  file_read: 'toolCall.displayName.fileView',
  view_image: 'toolCall.displayName.viewImage',
  image_generate: 'toolCall.displayName.imageGenerate',
  file_write: 'toolCall.displayName.fileCreate',
  file_patch: 'toolCall.displayName.fileEdit',
  apply_patch: 'toolCall.displayName.applyPatch',
  file_remove: 'toolCall.displayName.fileRemove',
  file_mkdir: 'toolCall.displayName.fileMkdir',
  file_copy: 'toolCall.displayName.fileCopy',
  file_move: 'toolCall.displayName.fileMove',
  search_text: 'toolCall.displayName.grepSearch',
  search_semantic: 'toolCall.displayName.codebaseRetrieval',
  process_inspect: 'toolCall.displayName.processInspect',
  diff_preview: 'toolCall.displayName.diffPreview',
  web_search: 'toolCall.displayName.webSearch',
  web_fetch: 'toolCall.displayName.webFetch',
  browser_navigate: 'toolCall.displayName.browserNavigate',
  browser_snapshot: 'toolCall.displayName.browserSnapshot',
  browser_click: 'toolCall.displayName.browserClick',
  browser_type: 'toolCall.displayName.browserType',
  browser_press: 'toolCall.displayName.browserPress',
  browser_scroll: 'toolCall.displayName.browserScroll',
  browser_screenshot: 'toolCall.displayName.browserScreenshot',
  browser_tabs: 'toolCall.displayName.browserTabs',
  browser_viewport: 'toolCall.displayName.browserViewport',
  browser_wait_for: 'toolCall.displayName.browserWaitFor',
  browser_hover: 'toolCall.displayName.browserHover',
  browser_drag: 'toolCall.displayName.browserDrag',
  browser_fill_form: 'toolCall.displayName.browserFillForm',
  browser_dialog: 'toolCall.displayName.browserDialog',
  browser_upload_file: 'toolCall.displayName.browserUploadFile',
  browser_click_at: 'toolCall.displayName.browserClickAt',
  browser_evaluate: 'toolCall.displayName.browserEvaluate',
  browser_console: 'toolCall.displayName.browserConsole',
  browser_network: 'toolCall.displayName.browserNetwork',
  browser_emulate: 'toolCall.displayName.browserEmulate',
  browser_performance: 'toolCall.displayName.browserPerformance',
  browser_lighthouse: 'toolCall.displayName.browserLighthouse',
  browser_heap: 'toolCall.displayName.browserHeap',
  browser_third_party: 'toolCall.displayName.browserThirdParty',
  browser_webmcp: 'toolCall.displayName.browserWebMcp',
  browser_pwa: 'toolCall.displayName.browserPwa',
  diagram_render: 'toolCall.displayName.diagramRender',
  knowledge_query: 'toolCall.displayName.knowledgeQuery',
  code_symbols: 'toolCall.displayName.codeSymbols',
  tool_catalog: 'toolCall.displayName.toolCatalog',
  agent_spawn: 'toolCall.displayName.agentSpawn',
  agent_wait: 'toolCall.displayName.agentWait',
  update_plan: 'toolCall.displayName.updatePlan',
  memory_write: 'toolCall.displayName.memoryWrite',
};

export function resolveToolDisplayName(toolName: string, translations: TranslationSource): string {
  if (!toolName || typeof toolName !== 'string') {
    return translations.t('toolCall.displayName.default');
  }
  const parsedTool = parseToolIdentity(toolName);
  if (parsedTool.source !== 'builtin') {
    return parsedTool.displayName;
  }
  const key = toolDisplayNameI18nKey(parsedTool.baseName);
  const translated = key ? translations.t(key) : '';
  return translated && translated !== key
    ? translated
    : formatToolNameFallback(parsedTool.baseName);
}

function toolDisplayNameI18nKey(name: string): string {
  const explicitKey = TOOL_DISPLAY_NAME_KEYS[name];
  if (explicitKey) return explicitKey;
  const suffix = name
    .split('_')
    .filter(Boolean)
    .map((part, index) => index === 0 ? part : part.charAt(0).toUpperCase() + part.slice(1))
    .join('');
  return suffix ? `toolCall.displayName.${suffix}` : '';
}

function formatToolNameFallback(name: string): string {
  const parts = name.split('_').map((part) => part.trim()).filter(Boolean);
  if (parts.length === 0) return name;
  return parts
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ');
}
