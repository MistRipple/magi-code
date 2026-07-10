import type {
  NotifyCategory,
  NotifyDisplayMode,
  NotifyLevel,
  NotifyPayload,
} from './protocol/message-protocol';

export interface ResolvedNotificationPresentation {
  level: NotifyLevel;
  displayMode: Exclude<NotifyDisplayMode, 'auto'>;
  category: NotifyCategory;
  source: string;
  actionRequired: boolean;
  title?: string;
  duration?: number;
}

function normalizeNotifyLevel(level: string | undefined): NotifyLevel {
  return level === 'success' || level === 'warning' || level === 'error' ? level : 'info';
}

function normalizeNotifyDisplayMode(
  displayMode: NotifyDisplayMode | undefined,
): 'toast' | 'silent' {
  if (displayMode === 'toast' || displayMode === 'silent') {
    return displayMode;
  }
  return 'toast';
}

function normalizeNotifyCategory(
  category: NotifyCategory | undefined,
  level: NotifyLevel,
): NotifyCategory {
  if (category === 'incident' || category === 'feedback') {
    return category;
  }
  return level === 'error' ? 'incident' : 'feedback';
}

export function resolveNotificationPresentation(
  notify: NotifyPayload | undefined,
  fallbackSource?: string,
): ResolvedNotificationPresentation {
  const level = normalizeNotifyLevel(notify?.level);
  const category = normalizeNotifyCategory(notify?.category, level);
  const displayMode = normalizeNotifyDisplayMode(notify?.displayMode);
  const source = typeof notify?.source === 'string' && notify.source.trim().length > 0
    ? notify.source.trim()
    : (typeof fallbackSource === 'string' && fallbackSource.trim().length > 0
      ? fallbackSource.trim()
      : 'model-runtime');
  const actionRequired = typeof notify?.actionRequired === 'boolean'
    ? notify.actionRequired
    : category === 'incident';

  return {
    level,
    displayMode,
    category,
    source,
    actionRequired,
    title: typeof notify?.title === 'string' ? notify.title : undefined,
    duration: typeof notify?.duration === 'number' ? notify.duration : undefined,
  };
}
