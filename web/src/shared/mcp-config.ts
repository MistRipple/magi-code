export type McpTransport = 'stdio' | 'streamable-http';

export type McpKeyValueRow = {
  key: string;
  value: string;
};

export type McpFormDraft = {
  name: string;
  type: McpTransport;
  enabled: boolean;
  command: string;
  args: string[];
  env: McpKeyValueRow[];
  url: string;
  headers: McpKeyValueRow[];
  requestTimeoutSeconds: string;
};

export type McpFormDraftConversionError =
  | 'missingName'
  | 'invalidTimeout'
  | 'emptyKey'
  | 'duplicateKey';

export type McpFormDraftConversionResult =
  | { ok: true; name: string; config: Record<string, unknown> }
  | { ok: false; error: McpFormDraftConversionError };

export type NormalizedMcpServerDraft = {
  id: string;
  name: string;
  type: McpTransport;
  enabled: boolean;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  requestTimeoutMs?: number;
};

export type McpServerDraftError =
  | 'unsupportedType'
  | 'missingCommand'
  | 'argsMustBeArray'
  | 'envMustBeObject'
  | 'missingUrl'
  | 'invalidUrl'
  | 'headersMustBeObject';

export type McpServerDraftResult =
  | { ok: true; server: NormalizedMcpServerDraft }
  | { ok: false; error: McpServerDraftError };

export function createMcpFormDraft(
  name = 'mcp-server',
  rawConfig: Record<string, unknown> = {},
): McpFormDraft {
  const type = resolveMcpTransport(
    stringValue(rawConfig.type).toLowerCase(),
    stringValue(rawConfig.url),
  ) ?? 'stdio';
  const timeout = rawConfig.requestTimeoutMs;

  return {
    name,
    type,
    enabled: rawConfig.enabled !== false,
    command: stringValue(rawConfig.command),
    args: Array.isArray(rawConfig.args)
      ? rawConfig.args.filter((value): value is string => typeof value === 'string')
      : [],
    env: recordToRows(rawConfig.env),
    url: stringValue(rawConfig.url),
    headers: recordToRows(rawConfig.headers),
    requestTimeoutSeconds: typeof timeout === 'number' && Number.isFinite(timeout)
      ? String(timeout / 1000)
      : '',
  };
}

export function convertMcpFormDraft(
  draft: McpFormDraft,
): McpFormDraftConversionResult {
  const name = draft.name.trim();
  if (!name) {
    return { ok: false, error: 'missingName' };
  }

  const timeoutText = draft.requestTimeoutSeconds.trim();
  let requestTimeoutMs: number | undefined;
  if (timeoutText) {
    const timeoutSeconds = Number(timeoutText);
    if (!Number.isFinite(timeoutSeconds) || timeoutSeconds < 1 || timeoutSeconds > 300) {
      return { ok: false, error: 'invalidTimeout' };
    }
    requestTimeoutMs = Math.round(timeoutSeconds * 1000);
  }

  const rows = draft.type === 'streamable-http' ? draft.headers : draft.env;
  const rowResult = rowsToRecord(rows);
  if (!rowResult.ok) {
    return rowResult;
  }

  if (draft.type === 'streamable-http') {
    return {
      ok: true,
      name,
      config: {
        type: 'streamable-http',
        url: draft.url.trim(),
        headers: rowResult.value,
        enabled: draft.enabled,
        ...(requestTimeoutMs === undefined ? {} : { requestTimeoutMs }),
      },
    };
  }

  return {
    ok: true,
    name,
    config: {
      type: 'stdio',
      command: draft.command.trim(),
      args: [...draft.args],
      env: rowResult.value,
      enabled: draft.enabled,
      ...(requestTimeoutMs === undefined ? {} : { requestTimeoutMs }),
    },
  };
}

export function normalizeMcpServerDraft(
  name: string,
  rawConfig: Record<string, unknown>,
): McpServerDraftResult {
  const rawType = stringValue(rawConfig.type).toLowerCase();
  const url = stringValue(rawConfig.url);
  const transport = resolveMcpTransport(rawType, url);
  if (!transport) {
    return { ok: false, error: 'unsupportedType' };
  }

  const base = {
    id: name,
    name,
    type: transport,
    enabled: rawConfig.enabled !== false,
  } satisfies Pick<NormalizedMcpServerDraft, 'id' | 'name' | 'type' | 'enabled'>;

  if (transport === 'streamable-http') {
    if (!url) {
      return { ok: false, error: 'missingUrl' };
    }
    if (!isHttpUrl(url)) {
      return { ok: false, error: 'invalidUrl' };
    }
    const headers = rawConfig.headers ?? {};
    if (!isStringRecord(headers)) {
      return { ok: false, error: 'headersMustBeObject' };
    }
    const requestTimeoutMs = normalizeRequestTimeout(rawConfig.requestTimeoutMs);
    return {
      ok: true,
      server: {
        ...base,
        url,
        headers,
        ...(requestTimeoutMs === undefined ? {} : { requestTimeoutMs }),
      },
    };
  }

  const command = stringValue(rawConfig.command);
  if (!command) {
    return { ok: false, error: 'missingCommand' };
  }
  const args = rawConfig.args ?? [];
  if (!Array.isArray(args) || args.some((value) => typeof value !== 'string')) {
    return { ok: false, error: 'argsMustBeArray' };
  }
  const env = rawConfig.env ?? {};
  if (!isStringRecord(env)) {
    return { ok: false, error: 'envMustBeObject' };
  }
  const requestTimeoutMs = normalizeRequestTimeout(rawConfig.requestTimeoutMs);
  return {
    ok: true,
    server: {
      ...base,
      command,
      args,
      env,
      ...(requestTimeoutMs === undefined ? {} : { requestTimeoutMs }),
    },
  };
}

function resolveMcpTransport(rawType: string, url: string): McpTransport | null {
  if (!rawType) {
    return url ? 'streamable-http' : 'stdio';
  }
  if (rawType === 'stdio') {
    return 'stdio';
  }
  if (rawType === 'http' || rawType === 'streamable-http' || rawType === 'streamable_http') {
    return 'streamable-http';
  }
  return null;
}

function stringValue(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function recordToRows(value: unknown): McpKeyValueRow[] {
  if (!isStringRecord(value)) {
    return [];
  }
  return Object.entries(value).map(([key, rowValue]) => ({ key, value: rowValue }));
}

function rowsToRecord(
  rows: McpKeyValueRow[],
): { ok: true; value: Record<string, string> } | { ok: false; error: 'emptyKey' | 'duplicateKey' } {
  const result: Record<string, string> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key && !row.value) {
      continue;
    }
    if (!key) {
      return { ok: false, error: 'emptyKey' };
    }
    if (Object.prototype.hasOwnProperty.call(result, key)) {
      return { ok: false, error: 'duplicateKey' };
    }
    result[key] = row.value;
  }
  return { ok: true, value: result };
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return Boolean(value)
    && typeof value === 'object'
    && !Array.isArray(value)
    && Object.values(value as Record<string, unknown>).every((item) => typeof item === 'string');
}

function isHttpUrl(value: string): boolean {
  try {
    const parsed = new URL(value);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

function normalizeRequestTimeout(value: unknown): number | undefined {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return undefined;
  }
  return Math.min(300_000, Math.max(1_000, Math.round(value)));
}
