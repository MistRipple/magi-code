/**
 * ID 生成器
 *
 * 使用 UUID v4 生成唯一 ID，完全随机，无碰撞风险
 */

import { v4 as uuidv4 } from 'uuid';

export class IDGenerator {
  /**
   * 生成完整 ID
   */
  static generate(prefix?: string): string {
    const id = uuidv4();
    return prefix ? `${prefix}_${id}` : id;
  }

  /**
   * 生成短 ID (用于显示)
   */
  static generateShort(prefix?: string): string {
    const id = uuidv4().split('-')[0];
    return prefix ? `${prefix}_${id}` : id;
  }

  /**
   * 生成带时间戳的 ID (用于排序)
   */
  static generateWithTimestamp(prefix?: string): string {
    const timestamp = Date.now();
    const id = uuidv4();
    return prefix ? `${prefix}_${timestamp}_${id}` : `${timestamp}_${id}`;
  }

  /**
   * 验证 ID 格式
   */
  static isValid(id: string): boolean {
    if (!id) return false;

    // UUID v4 格式
    const uuidRegex = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
    if (uuidRegex.test(id)) return true;

    // 带前缀的格式
    const prefixRegex = /^[a-z_]+_(.+)$/i;
    if (prefixRegex.test(id)) {
      const match = id.match(prefixRegex);
      if (match) {
        return this.isValid(match[1]);
      }
    }

    return false;
  }

  /**
   * 从 ID 中提取时间戳 (如果有)
   */
  static extractTimestamp(id: string): number | null {
    // 移除前缀
    const parts = id.split('_');

    // 查找时间戳部分
    for (const part of parts) {
      const timestamp = parseInt(part, 10);
      if (!isNaN(timestamp) && timestamp > 1000000000000) {
        return timestamp;
      }
    }

    return null;
  }
}
