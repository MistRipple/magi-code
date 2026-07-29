export const TOOL_CALL_FAILURE_SCHEMA_VERSION = 'tool-call-failure.v1' as const;

export interface ToolCallFailureDiagnostic {
  schemaVersion: typeof TOOL_CALL_FAILURE_SCHEMA_VERSION;
  code: string;
  summary: string;
  detail: string;
  stage: 'tool_call_validation';
  toolName: string;
  reasonCode: string;
  missingFields: string[];
  argumentsPreview: string;
  retryAttempts: number;
}

function requiredText(record: Record<string, unknown>, key: string): string {
  const value = record[key];
  return typeof value === 'string' ? value.trim() : '';
}

export function parseToolCallFailureDiagnostic(value: unknown): ToolCallFailureDiagnostic | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  if (record.schemaVersion !== TOOL_CALL_FAILURE_SCHEMA_VERSION) return null;
  const code = requiredText(record, 'code');
  const summary = requiredText(record, 'summary');
  const detail = requiredText(record, 'detail');
  const toolName = requiredText(record, 'toolName');
  const reasonCode = requiredText(record, 'reasonCode');
  const argumentsPreview = requiredText(record, 'argumentsPreview');
  const retryAttempts = record.retryAttempts;
  const missingFields = Array.isArray(record.missingFields)
    ? record.missingFields.filter((field): field is string => typeof field === 'string' && !!field.trim())
    : [];
  if (
    !code
    || !summary
    || !detail
    || !toolName
    || !reasonCode
    || !argumentsPreview
    || record.stage !== 'tool_call_validation'
    || typeof retryAttempts !== 'number'
    || !Number.isFinite(retryAttempts)
    || retryAttempts < 0
  ) {
    return null;
  }
  return {
    schemaVersion: TOOL_CALL_FAILURE_SCHEMA_VERSION,
    code,
    summary,
    detail,
    stage: 'tool_call_validation',
    toolName,
    reasonCode,
    missingFields,
    argumentsPreview,
    retryAttempts: Math.floor(retryAttempts),
  };
}
