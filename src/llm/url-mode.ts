import { LLMProvider, UrlMode } from '../types/agent-types';

const VERSION_SUFFIX_REGEX = /\/v\d+$/i;

export function normalizeUrlMode(value: unknown, fallback: UrlMode = 'standard'): UrlMode {
  if (value === 'full' || value === 'standard') {
    return value;
  }
  return fallback;
}

function trimBaseUrl(baseUrl: string): string {
  return typeof baseUrl === 'string' ? baseUrl.trim() : '';
}

function stripTrailingSlash(value: string): string {
  return value.replace(/\/+$/, '');
}

export function resolveSdkBaseUrl(provider: LLMProvider, baseUrl: string, urlMode: UrlMode = 'standard'): string {
  const trimmed = trimBaseUrl(baseUrl);
  if (!trimmed) {
    return '';
  }
  if (urlMode === 'full') {
    return trimmed;
  }

  const normalized = stripTrailingSlash(trimmed);
  if (provider === 'anthropic') {
    if (VERSION_SUFFIX_REGEX.test(normalized)) {
      return normalized.replace(VERSION_SUFFIX_REGEX, '');
    }
    return normalized;
  }

  if (VERSION_SUFFIX_REGEX.test(normalized)) {
    return normalized;
  }
  return `${normalized}/v1`;
}

export function resolveModelsBaseUrl(provider: LLMProvider, baseUrl: string, urlMode: UrlMode = 'standard'): string | null {
  if (urlMode === 'full') {
    return null;
  }

  const sdkBaseUrl = resolveSdkBaseUrl(provider, baseUrl, 'standard');
  if (!sdkBaseUrl) {
    return sdkBaseUrl;
  }

  if (provider === 'anthropic') {
    return `${sdkBaseUrl}/v1`;
  }

  return sdkBaseUrl;
}
