import type { NotifyCategory, NotifyDisplayMode, NotifyLevel, NotifyPresentation } from '../../protocol/message-protocol';

interface InternalControlNoticePublisher {
  notify(
    content: string,
    level?: NotifyLevel,
    duration?: number,
    presentation?: NotifyPresentation,
  ): void;
}

export interface InternalControlNoticeOptions {
  title: string;
  level?: NotifyLevel;
  category?: NotifyCategory;
  source?: string;
  displayMode?: Exclude<NotifyDisplayMode, 'auto'>;
  actionRequired?: boolean;
  countUnread?: boolean;
  persistToCenter?: boolean;
}

function normalizeNoticeContent(content: string): string {
  return content.replace(/^\[System\]\s*/i, '').trim();
}

export function publishInternalControlNotice(
  publisher: InternalControlNoticePublisher,
  content: string,
  options: InternalControlNoticeOptions,
): void {
  const normalizedContent = normalizeNoticeContent(content);
  if (!normalizedContent) {
    return;
  }

  const level = options.level ?? 'warning';
  publisher.notify(normalizedContent, level, undefined, {
    title: options.title,
    displayMode: options.displayMode ?? 'notification_center',
    category: options.category ?? (level === 'error' ? 'incident' : 'audit'),
    source: options.source ?? 'orchestration-runtime',
    persistToCenter: options.persistToCenter ?? true,
    actionRequired: options.actionRequired ?? (level === 'error'),
    countUnread: options.countUnread ?? false,
  });
}
