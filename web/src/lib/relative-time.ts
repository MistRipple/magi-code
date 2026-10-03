/** 允许时间戳比「当前时间」超前的范围：覆盖调用方的取整和本机时钟的微小误差。 */
const FUTURE_TOLERANCE_MS = 60_000;

export function formatRelativeTime(
  timestamp: string | number | Date | null | undefined,
  now = Date.now(),
  locale = '',
): string {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  const ms = now - date.getTime();
  if (Number.isNaN(ms) || ms < -FUTURE_TOLERANCE_MS) {
    return date.toLocaleDateString(locale, { month: 'short', day: 'numeric' });
  }
  const isZh = locale.toLowerCase().startsWith('zh');
  // 时间戳比「当前时间」略新不是真的在未来：调用方会把当前时间向下取整以降低刷新频率
  // （侧栏取整到 15 秒），正在运行的会话每秒都在更新，最近一次更新就会比取整后的当前时间新。
  // 这种情况按「刚刚」显示；只有明显超前才退回绝对日期。
  const minutes = Math.max(0, Math.floor(ms / 60000));
  if (minutes < 1) return isZh ? '刚刚' : 'just now';
  if (minutes < 60) return isZh ? `${minutes} 分钟` : `${minutes}m`;
  const hours = Math.floor(ms / 3600000);
  if (hours < 24) return isZh ? `${hours} 小时` : `${hours}h`;
  const days = Math.floor(ms / 86400000);
  if (days < 30) return isZh ? `${days} 天` : `${days}d`;
  return date.toLocaleDateString(locale, { month: 'short', day: 'numeric' });
}
