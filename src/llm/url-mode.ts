import { LLMProvider, UrlMode } from '../types/agent-types';
import type { ProtocolId } from './protocol/types';

const VERSION_SUFFIX_REGEX = /\/v\d+$/i;

/**
 * SDK 在 baseURL 之后硬编码追加的端点路径。
 * 来源：node_modules/openai 和 node_modules/@anthropic-ai/sdk 源码。
 *
 * 当 urlMode=full 时需要反向剥离，使 "用户提供的 URL = 最终请求 URL"。
 */
const ENDPOINT_TO_PROTOCOL: Array<{ suffix: string; protocol: ProtocolId }> = [
  { suffix: '/chat/completions', protocol: 'openai.chat-completions' },
  { suffix: '/responses',        protocol: 'openai.responses' },
  { suffix: '/v1/messages',      protocol: 'anthropic.messages' },
];

/**
 * full 模式下，从 URL 末尾推断实际协议。
 * URL 是权威真相 — 协议端点选择不会覆盖用户指定的完整路径。
 *
 * 返回 undefined 表示 URL 不以已知端点后缀结尾，应使用配置中的协议。
 */
export function inferProtocolFromFullUrl(
  baseUrl: string,
  urlMode: UrlMode,
): ProtocolId | undefined {
  if (urlMode !== 'full') {
    return undefined;
  }
  const normalized = stripTrailingSlash(trimBaseUrl(baseUrl));
  for (const entry of ENDPOINT_TO_PROTOCOL) {
    if (normalized.endsWith(entry.suffix)) {
      return entry.protocol;
    }
  }
  return undefined;
}

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

/**
 * full 模式下，检测 URL 末尾是否包含 SDK 会自动追加的端点后缀，
 * 如果包含则剥离，使 SDK 追加后的最终 URL 等于用户的原始输入。
 */
function stripSdkEndpointSuffix(url: string): string {
  const normalized = stripTrailingSlash(url);
  for (const entry of ENDPOINT_TO_PROTOCOL) {
    if (normalized.endsWith(entry.suffix)) {
      return normalized.slice(0, -entry.suffix.length);
    }
  }
  // 用户填的不以已知端点后缀结尾 → 视作 SDK baseURL，原样返回
  return normalized;
}

export function resolveSdkBaseUrl(provider: LLMProvider, baseUrl: string, urlMode: UrlMode = 'standard'): string {
  const trimmed = trimBaseUrl(baseUrl);
  if (!trimmed) {
    return '';
  }
  if (urlMode === 'full') {
    return stripSdkEndpointSuffix(trimmed);
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
