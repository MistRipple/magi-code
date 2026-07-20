/**
 * 工具函数
 */

/**
 * HTML 转义
 */
export function escapeHtml(str: string): string {
  if (!str) return '';
  const map: Record<string, string> = {
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#039;',
  };
  return str.replace(/[&<>"']/g, (m) => map[m] || m);
}

/**
 * 生成唯一 ID
 */
export function generateId(): string {
  return `${Date.now()}_${Math.random().toString(36).substring(2, 11)}`;
}

/**
 * 格式化用于消息溯源的本地时间：当天只显示时分，跨天显示完整日期与时分。
 */
export function formatTraceableTime(
  timestamp: number,
  referenceTimestamp = Date.now(),
): string {
  if (!Number.isFinite(timestamp)) return '--';
  const date = new Date(timestamp);
  const referenceDate = new Date(referenceTimestamp);
  if (Number.isNaN(date.getTime()) || Number.isNaN(referenceDate.getTime())) return '--';

  const pad2 = (value: number) => String(value).padStart(2, '0');
  const time = `${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
  const isSameDay = date.getFullYear() === referenceDate.getFullYear()
    && date.getMonth() === referenceDate.getMonth()
    && date.getDate() === referenceDate.getDate();
  if (isSameDay) return time;

  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())} ${time}`;
}

/**
 * 格式化持续时间
 */
export function formatDuration(ms: number): string {
  const totalSeconds = ms <= 0 ? 0 : Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  const pad2 = (value: number) => String(value).padStart(2, '0');

  if (hours > 0) {
    return `${hours}h${pad2(minutes)}m${pad2(seconds)}s`;
  }
  if (minutes > 0) {
    return `${minutes}m${pad2(seconds)}s`;
  }
  return `${seconds}s`;
}

/**
 * 防抖函数
 */
export function debounce<T extends (...args: unknown[]) => void>(
  fn: T,
  delay: number
): (...args: Parameters<T>) => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return (...args: Parameters<T>) => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => fn(...args), delay);
  };
}

/**
 * 节流函数
 */
export function throttle<T extends (...args: unknown[]) => void>(
  fn: T,
  limit: number
): (...args: Parameters<T>) => void {
  let lastCall = 0;
  return (...args: Parameters<T>) => {
    const now = Date.now();
    if (now - lastCall >= limit) {
      lastCall = now;
      fn(...args);
    }
  };
}

/**
 * 安全的 JSON 解析
 */
export function safeJsonParse<T>(str: string, defaultValue: T): T {
  try {
    return JSON.parse(str) as T;
  } catch {
    return defaultValue;
  }
}

/**
 * 格式化已用秒数（流式指示器专用，固定 MM:SS）
 */
export function formatElapsed(seconds: number): string {
  const normalizedSeconds = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(normalizedSeconds / 60);
  const remainingSeconds = normalizedSeconds % 60;
  return `${String(minutes).padStart(2, '0')}:${String(remainingSeconds).padStart(2, '0')}`;
}

/**
 * 截断文本
 */
export function truncate(str: string, maxLength: number): string {
  if (!str || str.length <= maxLength) return str;
  return str.substring(0, maxLength - 3) + '...';
}

/**
 * 确保值为数组
 */
export function ensureArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? value : [];
}
