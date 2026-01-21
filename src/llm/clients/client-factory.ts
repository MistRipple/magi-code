/**
 * LLM 客户端工厂
 */

import { LLMConfig } from '../../types/agent-types';
import { LLMClient } from '../types';
import { UniversalLLMClient } from './universal-client';

/**
 * 创建 LLM 客户端
 */
export function createLLMClient(config: LLMConfig): LLMClient {
  if (!config.enabled) {
    throw new Error('LLM client is disabled in config');
  }

  return new UniversalLLMClient(config);
}

/**
 * 客户端缓存
 */
const clientCache = new Map<string, LLMClient>();

/**
 * 获取或创建 LLM 客户端（带缓存）
 */
export function getOrCreateLLMClient(config: LLMConfig): LLMClient {
  const cacheKey = `${config.provider}-${config.model}-${config.baseUrl}`;

  let client = clientCache.get(cacheKey);
  if (!client) {
    client = createLLMClient(config);
    clientCache.set(cacheKey, client);
  }

  return client;
}

/**
 * 清除客户端缓存
 */
export function clearClientCache(): void {
  clientCache.clear();
}
