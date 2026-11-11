export function shouldShowRuntimePanel(status: string | undefined): boolean {
  return status === 'running'
    || status === 'waiting'
    || status === 'paused'
    || status === 'blocked'
    || status === 'failed';
}

export function shouldShowRuntimePhase(status: string | undefined, phase: string | undefined): boolean {
  const normalizedStatus = status?.trim();
  const normalizedPhase = phase?.trim();
  return Boolean(
    normalizedPhase
    && normalizedPhase !== 'idle'
    && normalizedPhase !== normalizedStatus,
  );
}
