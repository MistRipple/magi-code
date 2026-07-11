export type IncidentScope = 'app' | 'workspace' | 'session';

export interface FeedbackPolicy {
  category: 'feedback';
  persistToCenter: false;
  countUnread: false;
  actionRequired: false;
  displayMode: 'toast' | 'silent';
}

export interface IncidentPolicy {
  category: 'incident';
  persistToCenter: true;
  countUnread: true;
  actionRequired: true;
  displayMode: 'toast';
  scope: IncidentScope;
}

export interface IncidentContext {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

export interface IncidentReportInput {
  scope: IncidentScope;
  level: string;
  message: string;
  title?: string;
  source?: string;
  fingerprint?: string;
  actionRequired?: boolean;
  notificationId?: string;
}

export interface IncidentReportRequest extends IncidentReportInput {
  workspaceId: string;
  workspacePath?: string;
  sessionId?: string;
}

export interface NormalizedIncidentRecord {
  id: string;
  type: string;
  title?: string;
  message: string;
  scope: IncidentScope;
  workspaceId?: string;
  sessionId?: string;
  source?: string;
  actionRequired: boolean;
  countUnread: boolean;
  occurrenceCount: number;
  timestamp: number;
  read: boolean;
  resolved: boolean;
}

export function shouldDisplayToast(level: string): boolean {
  return level === 'warning' || level === 'error';
}

export function resolveFeedbackPolicy(level: string): FeedbackPolicy {
  return {
    category: 'feedback',
    persistToCenter: false,
    countUnread: false,
    actionRequired: false,
    displayMode: shouldDisplayToast(level) ? 'toast' : 'silent',
  };
}

export function resolveIncidentPolicy(
  options: { scope: IncidentScope },
): IncidentPolicy {
  return {
    category: 'incident',
    persistToCenter: true,
    countUnread: true,
    actionRequired: true,
    displayMode: 'toast',
    scope: options.scope,
  };
}

export function buildIncidentRequest(
  input: IncidentReportInput,
  context: IncidentContext,
): IncidentReportRequest {
  const workspaceId = optionalString(context.workspaceId);
  if (!workspaceId) {
    throw new Error('incident report requires workspaceId');
  }
  const sessionId = optionalString(context.sessionId);
  if (input.scope === 'session' && !sessionId) {
    throw new Error('session incident requires sessionId');
  }
  return {
    ...input,
    workspaceId,
    ...(optionalString(context.workspacePath)
      ? { workspacePath: optionalString(context.workspacePath) }
      : {}),
    ...(sessionId ? { sessionId } : {}),
  };
}

function optionalString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

function normalizeScope(value: unknown): IncidentScope | undefined {
  return value === 'app' || value === 'workspace' || value === 'session'
    ? value
    : undefined;
}

export function normalizeIncidentRecords(raw: unknown): NormalizedIncidentRecord[] {
  if (!Array.isArray(raw)) return [];
  const records: NormalizedIncidentRecord[] = [];
  const seen = new Set<string>();
  for (const item of raw) {
    if (!item || typeof item !== 'object') continue;
    const record = item as Record<string, unknown>;
    const id = optionalString(record.notificationId);
    const message = optionalString(record.message);
    const scope = normalizeScope(record.scope);
    if (!id || !message || !scope || seen.has(id)) continue;
    if (record.kind !== 'incident') continue;
    seen.add(id);
    records.push({
      id,
      type: optionalString(record.level) || 'error',
      title: optionalString(record.title),
      message,
      scope,
      workspaceId: optionalString(record.workspaceId),
      sessionId: optionalString(record.sessionId),
      source: optionalString(record.source),
      actionRequired: record.actionRequired !== false,
      countUnread: record.countUnread !== false,
      occurrenceCount: typeof record.occurrenceCount === 'number' && Number.isFinite(record.occurrenceCount)
        ? Math.max(1, Math.floor(record.occurrenceCount))
        : 1,
      timestamp: typeof record.createdAt === 'number' && Number.isFinite(record.createdAt)
        ? record.createdAt
        : Date.now(),
      read: typeof record.read === 'boolean' ? record.read : Boolean(record.handled),
      resolved: record.resolved === true,
    });
  }
  return records;
}
