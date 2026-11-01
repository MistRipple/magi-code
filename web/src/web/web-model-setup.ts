import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type { PluginManifest, WebModelSetupStatus, WebModelSetupCredentialsParams, WebModelSetupControlParams } from '../shared/app-server-protocol.generated';
import { WEB_MODEL_SETUP_COMMAND_ID } from '../lib/user-message-command';
import { WEB_MODEL_PLUGIN_ID } from '../shared/web-model';

export const WEB_MODEL_SETUP_COMMAND = WEB_MODEL_SETUP_COMMAND_ID;
export const WEB_MODEL_SETUP_REQUEST = 'magi:gptWebSetup';
export const WEB_MODEL_SETUP_UPDATED = 'magi:gptWebSetupUpdated';
export type SetupAction = 'start' | 'continue' | 'pause' | 'cancel';

/** 安装本身不注册辅助能力；只消费 daemon 已激活清单及命令投影。 */
export function webModelSetupAvailable(projection: {
  manifests: Pick<PluginManifest, 'id' | 'contributions'>[];
  commands: { id: string }[];
}): boolean {
  return projection.manifests.some(manifest => manifest.id === WEB_MODEL_PLUGIN_ID
    && manifest.contributions?.skills?.some(skill => skill.id === 'gpt-web-setup'));
}

/** 在普通聊天提交之前拦截意外粘贴的密钥；自然语言不被改写为命令。 */
export function containsSetupSecret(text: string): boolean { return /(?<![A-Za-z0-9_-])sk[-_][A-Za-z0-9_-]{8,}/u.test(text); }

export function setupCanEditCredentials(status: WebModelSetupStatus | null): boolean {
  return status?.allowedActions?.includes('save_credentials') === true;
}

export function setupIsActive(status: WebModelSetupStatus | null): boolean {
  return Boolean(status && !['completed', 'cancelled', 'revoked'].includes(status.runState ?? '') && !['complete', 'cancelled', 'busy'].includes(status.stage));
}

/** 必填规则和占位文案分别消费服务端保存事实，不能把“可稍后填写”当成已有密钥。 */
export function setupCredentialInput(stage: string, tunnelId: string, savedTunnelId: string, hasApiKey: boolean): { required: boolean; placeholderKey: string } {
  const canReuse = hasApiKey && tunnelId.trim() === savedTunnelId;
  return {
    required: (stage !== 'tunnel' || hasApiKey) && !canReuse,
    placeholderKey: canReuse ? 'webModel.setup.savedKeyPlaceholder'
      : stage === 'tunnel' && !hasApiKey ? 'webModel.setup.optionalKeyPlaceholder' : 'webModel.setup.keyPlaceholder',
  };
}

/** daemon 只保存固定原因；任意页面文本不会进入 reason。 */
export function setupReasonKey(reason: string): string | null {
  switch (reason) {
    case 'key_generation_required': return 'webModel.setup.reason.keyReady';
    case 'user_takeover': return 'webModel.setup.reason.userTakeover';
    default: return null;
  }
}

/** 一个检查点只展示一条主指引，人工交接不能同时声称正在自动处理。 */
export function setupHintKey(status: WebModelSetupStatus, busy: boolean, connectionFailed = false): string {
  if (busy) return 'webModel.setup.workingHint';
  if (status.reason === 'connector_attribution_pending') return 'webModel.setup.reason.connectorAttribution';
  if (status.continueState === 'saved_not_continued') return 'webModel.setup.continueFailed';
  if (status.stage === 'connecting' && status.continueState === 'waiting_channel') return 'webModel.setup.waitingChannel';
  if (status.stage === 'connecting' && connectionFailed) return 'webModel.setup.reason.connectionFailed';
  if (status.stage === 'credentials' && status.reason === 'runtime_key_required') return 'webModel.setup.reason.keyPreparation';
  return setupReasonKey(status.reason) ?? `webModel.setup.hint.${status.stage}`;
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
export async function saveWebModelSetupCredentials(request: WebModelSetupCredentialsParams): Promise<{ saved: boolean; status: string; configuration: WebModelSetupStatus }> {
  return read(await getTransport().request(agentUrl('/api/browser/web-models/setup/credentials'), {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify(request),
  }));
}

export async function controlWebModelSetup(request: WebModelSetupControlParams): Promise<{ configuration: WebModelSetupStatus }> {
  return read(await getTransport().request(agentUrl('/api/browser/web-models/setup/control'), {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(request),
  }));
}

/** 只查询脱敏回执，不重传秘密或创建继续输入。 */
export async function getWebModelSetupReceipt(sessionId: string, requestId: string): Promise<{ saved: boolean; status: string; configuration: WebModelSetupStatus }> {
  return read(await getTransport().request(agentUrl(`/api/browser/web-models/setup/credentials?sessionId=${encodeURIComponent(sessionId)}&requestId=${encodeURIComponent(requestId)}`), { cache: 'no-store' }));
}
