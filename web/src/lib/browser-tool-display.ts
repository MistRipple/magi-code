import { firstToolDisplayText, normalizeToolDisplayText } from './tool-call-display';
import { parseToolIdentity } from './tool-identity';

/** 浏览器工具的基础名（去掉命名空间），不是浏览器工具时返回空串。 */
function browserToolBaseName(toolName: string): string {
  const parsed = parseToolIdentity(toolName);
  const baseName = (parsed.baseName || toolName).toLowerCase();
  return baseName.startsWith('browser_') ? baseName : '';
}

function quoted(value: unknown): string {
  const text = normalizeToolDisplayText(value);
  return text ? `“${text}”` : '';
}

/**
 * 浏览器工具卡片标题旁的一行摘要：只展示对人有意义的参数（网址、输入内容、按键、
 * 检索词、尺寸……），不展示 e:3:12 这类只对模型有意义的元素引用。
 * 不是浏览器工具时返回 null，由调用方继续走通用摘要。
 */
export function browserToolSummary(
  toolName: string,
  args: Record<string, unknown>,
  output?: unknown,
): string | null {
  const baseName = browserToolBaseName(toolName);
  if (!baseName) return null;
  switch (baseName) {
    case 'browser_navigate':
      return firstToolDisplayText(args.url, args.action);
    case 'browser_click':
    case 'browser_hover':
      return actedTargetLabel(output);
    case 'browser_type': {
      const target = actedTargetLabel(output);
      return [quoted(args.text), target].filter(Boolean).join(' → ');
    }
    case 'browser_press':
      return normalizeToolDisplayText(args.key);
    case 'browser_read':
      return quoted(args.query);
    case 'browser_wait_for':
      return firstToolDisplayText(args.text, args.selector, args.url);
    case 'browser_tabs':
      return [normalizeToolDisplayText(args.action), normalizeToolDisplayText(args.url)].filter(Boolean).join(' ');
    case 'browser_viewport': {
      const width = typeof args.width === 'number' ? args.width : null;
      const height = typeof args.height === 'number' ? args.height : null;
      const size = width !== null && height !== null ? `${width}×${height}` : '';
      return [normalizeToolDisplayText(args.mode) || normalizeToolDisplayText(args.action), size].filter(Boolean).join(' ');
    }
    case 'browser_evaluate':
      return normalizeToolDisplayText(args.expression);
    case 'browser_storage':
      return [args.area, args.action, args.key].map(normalizeToolDisplayText).filter(Boolean).join(' ');
    case 'browser_download':
      // 只展示保存位置的文件名；list 没有可展示的目标。
      return normalizeToolDisplayText(args.destination_path).split(/[\\/]/u).pop() || normalizeToolDisplayText(args.action);
    case 'browser_upload_file': {
      const paths = Array.isArray(args.file_paths) ? args.file_paths : [args.file_path];
      return paths
        .map((path) => normalizeToolDisplayText(path).split(/[\\/]/u).pop() || '')
        .filter(Boolean)
        .join(', ');
    }
    case 'browser_drag':
    case 'browser_fill_form':
    case 'browser_snapshot':
    case 'browser_click_at':
    case 'browser_scroll':
    case 'browser_screenshot':
      return '';
    default:
      return normalizeToolDisplayText(args.action);
  }
}

/**
 * 交互结果里实际作用的元素（运行时报告的可访问名称，没有名称时用角色）。click/type 在结果
 * 顶层的 `target`，hover 这类 DevTools 操作在 `result.target`。结果未到达时返回空串。
 */
function actedTargetLabel(output: unknown): string {
  const payload = parseObject(output);
  if (!payload) return '';
  const nested = parseObject(payload.result);
  const target = parseObject(payload.target) ?? (nested ? parseObject(nested.target) : null);
  if (!target) return '';
  return normalizeToolDisplayText(target.name) || normalizeToolDisplayText(target.role);
}

function parseObject(content: unknown): Record<string, unknown> | null {
  if (content && typeof content === 'object' && !Array.isArray(content)) {
    return content as Record<string, unknown>;
  }
  if (typeof content !== 'string' || !content.trim().startsWith('{')) return null;
  try {
    const parsed = JSON.parse(content);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

export interface BrowserScreenshotPreview {
  /** artifact 的绝对路径（模型看到的同一个路径），仅用于展示。 */
  path: string;
  /** 会话 artifact 目录下的文件名，用于通过 `/api/browser/artifacts/{session}/{file}` 读取。 */
  fileName: string;
  mime: string;
  bytes: number | undefined;
}

/** browser_screenshot 成功结果里的截图 artifact；不是截图工具或结果不完整时返回 null。 */
export function parseBrowserScreenshotPreview(toolName: string, content: unknown): BrowserScreenshotPreview | null {
  if (browserToolBaseName(toolName) !== 'browser_screenshot') return null;
  const payload = parseObject(content);
  if (!payload || payload.status !== 'succeeded') return null;
  const path = typeof payload.path === 'string' ? payload.path.trim() : '';
  const mime = typeof payload.mime === 'string' ? payload.mime.trim() : '';
  const fileName = path.split(/[\\/]/u).pop() || '';
  if (!fileName || !mime.startsWith('image/')) return null;
  return {
    path,
    fileName,
    mime,
    bytes: typeof payload.bytes === 'number' && Number.isFinite(payload.bytes) ? payload.bytes : undefined,
  };
}

/**
 * browser_read 的结果在卡片里展示为读到的正文（或检索命中的上下文），
 * 而不是整段 JSON。不是 browser_read 或结果结构不符时返回 null，由调用方走通用格式化。
 */
export function formatBrowserReadToolOutput(toolName: string, content: unknown): string | null {
  if (browserToolBaseName(toolName) !== 'browser_read') return null;
  const payload = parseObject(content);
  const result = payload && parseObject(payload.result);
  if (!result) return null;
  if (Array.isArray(result.matches)) {
    const contexts = result.matches
      .map((match) => (match && typeof match === 'object' ? normalizeToolDisplayText((match as Record<string, unknown>).context) : ''))
      .filter(Boolean);
    return contexts.map((context) => `- …${context}…`).join('\n');
  }
  return typeof result.text === 'string' ? result.text : null;
}
