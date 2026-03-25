import type {
  NotifyCategory,
  NotifyDisplayMode,
  NotifyLevel,
  NotifyPayload,
} from '../protocol/message-protocol';

export interface ResolvedNotificationPresentation {
  level: NotifyLevel;
  displayMode: Exclude<NotifyDisplayMode, 'auto'>;
  category: NotifyCategory;
  source: string;
  actionRequired: boolean;
  persistToCenter: boolean;
  countUnread: boolean;
  title?: string;
  duration?: number;
}

function normalizeNotifyLevel(level: string | undefined): NotifyLevel {
  return level === 'success' || level === 'warning' || level === 'error' ? level : 'info';
}

function normalizeNotifyDisplayMode(
  displayMode: NotifyDisplayMode | undefined,
  level: NotifyLevel,
): Exclude<NotifyDisplayMode, 'auto'> {
  if (displayMode === 'toast' || displayMode === 'notification_center' || displayMode === 'silent') {
    return displayMode;
  }
  return level === 'error' ? 'toast' : 'notification_center';
}

function normalizeNotifyCategory(
  category: NotifyCategory | undefined,
  level: NotifyLevel,
): NotifyCategory {
  if (category === 'incident' || category === 'audit' || category === 'feedback') {
    return category;
  }
  return level === 'error' ? 'incident' : 'audit';
}

export function resolveNotificationPresentation(
  notify: NotifyPayload | undefined,
  fallbackSource?: string,
): ResolvedNotificationPresentation {
  const level = normalizeNotifyLevel(notify?.level);
  const category = normalizeNotifyCategory(notify?.category, level);
  const displayMode = normalizeNotifyDisplayMode(notify?.displayMode, level);
  const source = typeof notify?.source === 'string' && notify.source.trim().length > 0
    ? notify.source.trim()
    : (typeof fallbackSource === 'string' && fallbackSource.trim().length > 0
      ? fallbackSource.trim()
      : 'model-runtime');
  const persistToCenter = typeof notify?.persistToCenter === 'boolean'
    ? notify.persistToCenter
    : category !== 'feedback';
  const actionRequired = typeof notify?.actionRequired === 'boolean'
    ? notify.actionRequired
    : category === 'incident';
  const countUnread = persistToCenter
    ? (typeof notify?.countUnread === 'boolean' ? notify.countUnread : category === 'incident')
    : false;

  return {
    level,
    displayMode,
    category,
    source,
    actionRequired,
    persistToCenter,
    countUnread,
    title: typeof notify?.title === 'string' ? notify.title : undefined,
    duration: typeof notify?.duration === 'number' ? notify.duration : undefined,
  };
}

export function shouldPersistNotificationRecord(
  presentation: Pick<ResolvedNotificationPresentation, 'persistToCenter' | 'category'>,
): boolean {
  return presentation.persistToCenter && presentation.category !== 'feedback';
}
