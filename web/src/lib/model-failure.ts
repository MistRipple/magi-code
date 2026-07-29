export const MODEL_FAILURE_SCHEMA_VERSION = 'model-failure.v1' as const;

export interface ModelFailureDiagnostic {
  schemaVersion: typeof MODEL_FAILURE_SCHEMA_VERSION;
  code: string;
  summary: string;
  detail: string;
  stage: string;
  retryable: boolean;
  retryAttempts: number;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function requiredText(record: Record<string, unknown>, key: string): string {
  const value = record[key];
  return typeof value === 'string' ? value.trim() : '';
}

export function parseModelFailureDiagnostic(value: unknown): ModelFailureDiagnostic | null {
  const record = asRecord(value);
  if (!record || record.schemaVersion !== MODEL_FAILURE_SCHEMA_VERSION) {
    return null;
  }
  const code = requiredText(record, 'code');
  const summary = requiredText(record, 'summary');
  const detail = requiredText(record, 'detail');
  const stage = requiredText(record, 'stage');
  const retryAttempts = record.retryAttempts;
  if (
    !code
    || !summary
    || !detail
    || !stage
    || typeof record.retryable !== 'boolean'
    || typeof retryAttempts !== 'number'
    || !Number.isFinite(retryAttempts)
    || retryAttempts < 0
  ) {
    return null;
  }
  return {
    schemaVersion: MODEL_FAILURE_SCHEMA_VERSION,
    code,
    summary,
    detail,
    stage,
    retryable: record.retryable,
    retryAttempts: Math.floor(retryAttempts),
  };
}
