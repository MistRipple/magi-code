export function formatRelativeTime(
  timestamp: string | number | Date | null | undefined,
  now = Date.now(),
  locale = '',
): string {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  const ms = now - date.getTime();
  if (Number.isNaN(ms) || ms < 0) {
    return date.toLocaleDateString(locale, { month: 'short', day: 'numeric' });
  }
  const isZh = locale.toLowerCase().startsWith('zh');
  const minutes = Math.floor(ms / 60000);
  if (minutes < 1) return isZh ? '刚刚' : 'just now';
  if (minutes < 60) return isZh ? `${minutes} 分钟` : `${minutes}m`;
  const hours = Math.floor(ms / 3600000);
  if (hours < 24) return isZh ? `${hours} 小时` : `${hours}h`;
  const days = Math.floor(ms / 86400000);
  if (days < 30) return isZh ? `${days} 天` : `${days}d`;
  return date.toLocaleDateString(locale, { month: 'short', day: 'numeric' });
}
