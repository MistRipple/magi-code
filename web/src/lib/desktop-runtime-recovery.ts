import { isDesktopRuntime } from './desktop-updater';

export type DesktopRuntimeRecoveryStatus =
  | 'starting'
  | 'ready'
  | 'restarting'
  | 'port-occupied'
  | 'unavailable'
  | 'failed';

export interface DesktopPortOccupant {
  pid: number;
  processName: string;
  executablePath?: string;
  startedAt: number;
  isMagi: boolean;
}

export interface DesktopRuntimeRecoverySnapshot {
  status: DesktopRuntimeRecoveryStatus;
  port: number;
  technicalDetail?: string;
  occupants: DesktopPortOccupant[];
  canRestart: boolean;
  requiresConfirmation: boolean;
  webUrl?: string;
}

export async function getDesktopRuntimeRecovery(): Promise<DesktopRuntimeRecoverySnapshot | null> {
  if (!isDesktopRuntime()) {
    return null;
  }
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<DesktopRuntimeRecoverySnapshot>('get_desktop_runtime_recovery');
}

export async function restartDesktopRuntime(
  snapshot: DesktopRuntimeRecoverySnapshot,
  confirmExternalProcesses: boolean,
): Promise<DesktopRuntimeRecoverySnapshot> {
  if (!isDesktopRuntime()) {
    throw new Error('当前页面不在 Magi 桌面端中，无法管理本机服务进程');
  }
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<DesktopRuntimeRecoverySnapshot>('restart_desktop_runtime', {
    request: {
      expectedOccupants: snapshot.occupants.map((occupant) => ({
        pid: occupant.pid,
        startedAt: occupant.startedAt,
      })),
      confirmExternalProcesses,
    },
  });
}
