/**
 * 协议一致性校验器
 *
 * 目的：
 * - 统一校验 Provider 适配器输出的标准模型，防止非标准输出泄漏到编排层。
 * - 让新增 Provider 在接入阶段更早暴露协议偏差。
 */

import { LLMResponse, LLMStreamChunk } from '../types';

const VALID_STOP_REASONS = new Set<LLMResponse['stopReason']>([
  'end_turn',
  'max_tokens',
  'tool_use',
  'stop_sequence',
]);

const VALID_STREAM_TYPES = new Set<LLMStreamChunk['type']>([
  'content_start',
  'content_delta',
  'content_end',
  'tool_call_start',
  'tool_call_delta',
  'tool_call_end',
  'thinking',
  'usage',
]);

export class ProtocolConformanceValidator {
  validateResponse(response: LLMResponse, protocol: string): void {
    if (!response || typeof response !== 'object') {
      throw new Error(`[Conformance:${protocol}] response 为空或类型非法`);
    }
    if (typeof response.content !== 'string') {
      throw new Error(`[Conformance:${protocol}] response.content 必须是字符串`);
    }
    if (!response.usage || typeof response.usage !== 'object') {
      throw new Error(`[Conformance:${protocol}] response.usage 缺失`);
    }

    this.assertTokenNumber(response.usage.inputTokens, `[Conformance:${protocol}] usage.inputTokens 非法`);
    this.assertTokenNumber(response.usage.outputTokens, `[Conformance:${protocol}] usage.outputTokens 非法`);

    if (!VALID_STOP_REASONS.has(response.stopReason)) {
      throw new Error(`[Conformance:${protocol}] stopReason 非法: ${String(response.stopReason)}`);
    }

    if (response.toolCalls) {
      if (!Array.isArray(response.toolCalls)) {
        throw new Error(`[Conformance:${protocol}] toolCalls 必须是数组`);
      }
      for (const call of response.toolCalls) {
        if (!call || typeof call !== 'object') {
          throw new Error(`[Conformance:${protocol}] toolCall 结构非法`);
        }
        if (typeof call.id !== 'string' || !call.id.trim()) {
          throw new Error(`[Conformance:${protocol}] toolCall.id 不能为空`);
        }
        if (typeof call.name !== 'string' || !call.name.trim()) {
          throw new Error(`[Conformance:${protocol}] toolCall.name 不能为空`);
        }
        const isObjectArgs = !!call.arguments
          && typeof call.arguments === 'object'
          && !Array.isArray(call.arguments);
        const isParseErrorPlaceholder = call.arguments == null
          && typeof call.argumentParseError === 'string'
          && call.argumentParseError.length > 0;
        if (!isObjectArgs && !isParseErrorPlaceholder) {
          throw new Error(`[Conformance:${protocol}] toolCall.arguments 必须是对象`);
        }
      }
    }
  }

  validateStreamChunk(chunk: LLMStreamChunk, protocol: string): void {
    if (!chunk || typeof chunk !== 'object') {
      throw new Error(`[Conformance:${protocol}] stream chunk 为空或类型非法`);
    }
    if (!VALID_STREAM_TYPES.has(chunk.type)) {
      throw new Error(`[Conformance:${protocol}] stream chunk.type 非法: ${String(chunk.type)}`);
    }

    switch (chunk.type) {
      case 'content_delta':
        if (typeof chunk.content !== 'string') {
          throw new Error(`[Conformance:${protocol}] content_delta 缺少 content 字符串`);
        }
        break;
      case 'tool_call_start':
      case 'tool_call_delta':
      case 'tool_call_end':
        if (!chunk.toolCall || typeof chunk.toolCall !== 'object') {
          throw new Error(`[Conformance:${protocol}] ${chunk.type} 缺少 toolCall`);
        }
        break;
      case 'thinking':
        if (typeof chunk.thinking !== 'string') {
          throw new Error(`[Conformance:${protocol}] thinking 缺少 thinking 字符串`);
        }
        break;
      case 'usage':
        if (!chunk.usage || typeof chunk.usage !== 'object') {
          throw new Error(`[Conformance:${protocol}] usage chunk 缺少 usage 对象`);
        }
        if (chunk.usage.inputTokens !== undefined) {
          this.assertTokenNumber(chunk.usage.inputTokens, `[Conformance:${protocol}] usage.inputTokens 非法`);
        }
        if (chunk.usage.outputTokens !== undefined) {
          this.assertTokenNumber(chunk.usage.outputTokens, `[Conformance:${protocol}] usage.outputTokens 非法`);
        }
        break;
      default:
        break;
    }
  }

  private assertTokenNumber(value: unknown, message: string): void {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) {
      throw new Error(message);
    }
  }
}
