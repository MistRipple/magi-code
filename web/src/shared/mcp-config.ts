export type McpTransport = 'stdio' | 'streamable-http';

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
