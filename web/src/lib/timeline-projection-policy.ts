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

function projectionContainsCurrentTurn(projection: SessionTimelineProjection | null | undefined): boolean {
  const artifacts = Array.isArray(projection?.artifacts) ? projection.artifacts : [];
  return artifacts.some((artifact) => {
    const metadata = artifact?.message?.metadata;
    const turnId = metadata && typeof metadata === 'object' && typeof metadata.turnId === 'string'
      ? metadata.turnId.trim()
      : '';
    if (turnId) {
      return true;
    }
    const executionItems = Array.isArray(artifact?.executionItems) ? artifact.executionItems : [];
    return executionItems.some((item) => {
      const itemMetadata = item?.message?.metadata;
      return Boolean(
        itemMetadata
        && typeof itemMetadata === 'object'
        && typeof itemMetadata.turnId === 'string'
        && itemMetadata.turnId.trim(),
      );
    });
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

  const incomingHasCurrentTurn = projectionContainsCurrentTurn(incomingProjection);
  const currentHasCurrentTurn = projectionContainsCurrentTurn(currentProjection);

  // 当前架构下，current_turn 是主线编排唯一宿主。
  // 同 session 的 live 节点如果还停留在旧 projection，authoritative projection
  // 一旦带有 current_turn，就必须优先接管，避免主线顺序继续受旧宿主影响。
  if (incomingHasCurrentTurn && !currentHasCurrentTurn) {
    return true;
  }
  if (currentHasCurrentTurn && !incomingHasCurrentTurn) {
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
