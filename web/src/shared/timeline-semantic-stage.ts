/**
 * 时间轴投影排序的语义阶段号（兜底排序因子）。
 *
 * 排序主逻辑按 turnSeq / itemSeq / laneSeq / eventSeq 等事实序号排列。
 * 本函数仅用于旧消息缺少事实序号时的语义分层，不参与用时间戳猜顺序。
 * 数字越小越靠前。
 *
 * 此函数由后端投影构建（session-timeline-projection.ts）
 * 和前端渲染排序（timeline-render-items.ts、messages.svelte.ts）共同使用。
 * 接口设计为纯值参数，不依赖任何消息类型定义。
 *
 * 兜底阶段定义：
 *   0  — 用户输入（user_input）
 *   5  — 系统通知（system-notice）
 *  10  — 编排器思考（thinking）
 *  15  — 编排器主线回复（text）
 *  20  — 编排器派发（worker_dispatch）
 *  25  — Worker 指令/完成卡片（instruction / task_card）
 *  40  — 编排器收集结果（worker_wait）
 *  50  — 其他（默认）
 */
export function resolveMessageSemanticStage(
  messageType: string | undefined,
  primaryToolName: string,
): number {
  if (messageType === 'user_input') {
    return 0;
  }
  if (messageType === 'system-notice') {
    return 5;
  }
  if (messageType === 'thinking') {
    return 10;
  }
  if (primaryToolName === 'worker_dispatch') {
    return 20;
  }
  if (messageType === 'instruction' || messageType === 'task_card') {
    return 25;
  }
  if (primaryToolName === 'worker_wait') {
    return 40;
  }
  return 50;
}
