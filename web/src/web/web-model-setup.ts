import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type { WebModelSetupStatus } from '../shared/app-server-protocol.generated';
import { WEB_MODEL_SETUP_COMMAND_ID } from '../lib/user-message-command';

export const WEB_MODEL_SETUP_COMMAND = WEB_MODEL_SETUP_COMMAND_ID;
export const WEB_MODEL_SETUP_REQUEST = 'magi:gptWebSetup';
export type SetupAction = 'start' | 'continue' | 'confirm_connector' | 'cancel';

export function setupIsActive(status: WebModelSetupStatus | null): boolean {
  return Boolean(status && !['complete', 'cancelled', 'busy'].includes(status.stage));
}

export function setupNeedsConnectorConsent(status: WebModelSetupStatus | null): boolean {
  return status?.stage === 'connector' && status.reason === 'confirmation_required';
}

/** 页面只能返回固定原因；显示具体操作提示，不展示任意页面诊断。 */
export function setupReasonKey(reason: string): string | null {
  switch (reason) {
    case 'login_required': return 'webModel.setup.reason.platformLogin';
    case 'page_required': return 'webModel.setup.reason.platformPage';
    case 'ambiguous_tunnel': return 'webModel.setup.reason.ambiguousTunnel';
    case 'tunnel_id_required': return 'webModel.setup.reason.tunnelId';
    case 'permission_or_page_required': return 'webModel.setup.reason.permissions';
    case 'creation_started':
    case 'creation_unconfirmed': return 'webModel.setup.reason.tunnelUnconfirmed';
    case 'connector_write_started':
    case 'connector_unconfirmed': return 'webModel.setup.reason.connectorUnconfirmed';
    case 'manual_required': return 'webModel.setup.reason.manual';
    default: return null;
  }
}

async function read<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { message?: string } | null;
    throw new Error(body?.message || `HTTP ${response.status}`);
  }
  return response.json() as Promise<T>;
}

export async function getWebModelSetup(sessionId: string): Promise<WebModelSetupStatus | null> {
  return read(await getTransport().request(agentUrl(`/api/browser/web-models/setup?sessionId=${encodeURIComponent(sessionId)}`), { cache: 'no-store' }));
}

/** 凭据单独提交，不经过 executeTask、Turn、模型或浏览器工具。 */
export async function saveWebModelSetupCredentials(sessionId: string, tunnelId: string, apiKey: string): Promise<void> {
  await read(await getTransport().request(agentUrl('/api/browser/web-models/setup/credentials'), {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ sessionId, tunnelId: tunnelId.trim() || null, apiKey }),
  }));
}
