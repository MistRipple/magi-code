import type { SessionTimelineProjection } from '../types/message';
import {
  compareTimelineProjectionFreshness,
  compareTimelineProjectionRichness,
  shouldPreferRicherAuthoritativeProjection,
} from './timeline-projection-freshness';

export interface TimelineProjectionHydrationInput {
  currentSessionId: string | null | undefined;
  incomingSessionId: string | null | undefined;
  localTimelineNodeCount: number;
  localProjectionSource?: 'none' | 'persisted' | 'live' | 'authoritative';
  currentTimelineProjection?: SessionTimelineProjection | null | undefined;
  incomingTimelineProjection?: SessionTimelineProjection | null | undefined;
}

function projectionContainsDispatchGroup(projection: SessionTimelineProjection | null | undefined): boolean {
  const artifacts = Array.isArray(projection?.artifacts) ? projection.artifacts : [];
  return artifacts.some((artifact) => {
    const blocks = Array.isArray(artifact?.message?.blocks) ? artifact.message.blocks : [];
    return blocks.some((block) => block?.type === 'dispatch_group');
  });
}


export function shouldAcceptAuthoritativeTimelineProjection(
  input: TimelineProjectionHydrationInput,
): boolean {
  const currentSessionId = typeof input.currentSessionId === 'string'
    ? input.currentSessionId.trim()
    : '';
  const incomingSessionId = typeof input.incomingSessionId === 'string'
    ? input.incomingSessionId.trim()
    : '';

  if (!currentSessionId || !incomingSessionId || currentSessionId !== incomingSessionId) {
    return false;
  }

  if (input.localTimelineNodeCount <= 0) {
    return true;
  }
  const incomingProjection = input.incomingTimelineProjection;
  const currentProjection = input.currentTimelineProjection;
  if (!incomingProjection) {
    return false;
  }

  const currentProjectionSessionId = typeof currentProjection?.sessionId === 'string'
    ? currentProjection.sessionId.trim()
    : '';
  if (currentProjectionSessionId && currentProjectionSessionId !== incomingSessionId) {
    return true;
  }

  if (!currentProjection) {
    return true;
  }

  const incomingHasDispatchGroup = projectionContainsDispatchGroup(incomingProjection);
  const currentHasDispatchGroup = projectionContainsDispatchGroup(currentProjection);

  // 架构约束：dispatch_group 是 Worker 生命周期主线唯一宿主。
  // 同 session 的 live 文本节点若尚未切换到 dispatch_group，
  // authoritative projection 一旦包含 dispatch_group，必须允许其接管，
  // 避免出现“派发后主线先无卡片、结束后卡片仍丢失”的退化体验。
  if (incomingHasDispatchGroup && !currentHasDispatchGroup) {
    return true;
  }
  if (currentHasDispatchGroup && !incomingHasDispatchGroup) {
    return false;
  }

  const incomingStrictlyNewer = compareTimelineProjectionFreshness(incomingProjection, currentProjection) > 0;
  const currentStrictlyNewer = compareTimelineProjectionFreshness(incomingProjection, currentProjection) < 0;
  const incomingRicher = shouldPreferRicherAuthoritativeProjection(incomingProjection, currentProjection);
  const currentRicher = shouldPreferRicherAuthoritativeProjection(currentProjection, incomingProjection);
  const richnessComparison = compareTimelineProjectionRichness(incomingProjection, currentProjection);

  // authoritative projection 的首要职责是把后端最新真相带到当前页面。
  // 同 session 下，一旦 incoming projection 的 freshness 更高，必须优先接管；
  // 否则 local live 节点因为暂时“更胖”（例如含 worker-only 细节）会压住更新后的主线结果，
  // 导致“刷新后能看到 summary，live 页面却看不到”的退化。
  if (incomingStrictlyNewer) {
    return true;
  }

  if (currentStrictlyNewer) {
    return false;
  }

  if (incomingRicher || richnessComparison > 0) {
    return true;
  }

  if (currentRicher || richnessComparison < 0) {
    return false;
  }

  return false;
}

export function shouldHydrateAuthoritativeTimelineProjection(
  input: TimelineProjectionHydrationInput,
): boolean {
  return shouldAcceptAuthoritativeTimelineProjection(input);
}
