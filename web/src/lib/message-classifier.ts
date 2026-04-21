/**
 * Worker slot 归一化工具
 *
 * 唯一实现在 shared/timeline-worker-lifecycle.ts，此处仅重新导出。
 * 返回类型为 string（空字符串表示非 worker），所有调用方使用 falsy 检查兼容。
 */
export { normalizeWorkerSlot } from '../shared/timeline-worker-lifecycle';
