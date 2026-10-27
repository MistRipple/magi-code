import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type { PluginManifest, WebModelSetupStatus } from '../shared/app-server-protocol.generated';
import { WEB_MODEL_SETUP_COMMAND_ID } from '../lib/user-message-command';
import { WEB_MODEL_PLUGIN_ID } from '../shared/web-model';

export const WEB_MODEL_SETUP_COMMAND = WEB_MODEL_SETUP_COMMAND_ID;
export const WEB_MODEL_SETUP_REQUEST = 'magi:gptWebSetup';
export type SetupAction = 'start' | 'continue' | 'retry_tunnel' | 'confirm_connector' | 'cancel';

/** 安装本身不注册辅助能力；只消费 daemon 已激活清单及命令投影。 */
export function webModelSetupAvailable(projection: {
  manifests: Pick<PluginManifest, 'id'>[];
  commands: { id: string }[];
}): boolean {
  return projection.manifests.some(manifest => manifest.id === WEB_MODEL_PLUGIN_ID)
    && projection.commands.some(command => command.id === WEB_MODEL_SETUP_COMMAND);
}

/** 插件未贡献配置命令时保留普通消息语义；讨论、否定或引用也不触发账号操作。 */
export function isWebModelSetupRequest(text: string, available: boolean): boolean {
  return available && /^(?:(?:请|麻烦)?(?:帮我|帮忙|协助我)?(?:开始|进行)?(?:配置|设置|绑定|连接)(?:一下)?\s*(?:magi\s*(?:的)?)?gpt\s*web(?:\s*插件)?(?:\s*的)?(?:\s*(?:工具通道|连接器))?(?:吧|。|！|!)?|(?:please\s+)?(?:help me\s+)?(?:set up|configure|connect)\s+gpt\s*web[.!]?)$/iu.test(text.trim());
}

export function setupCanEditCredentials(status: WebModelSetupStatus | null): boolean {
  return Boolean(status && (['tunnel', 'credentials', 'connecting'].includes(status.stage)
    || (status.stage === 'connector' && status.reason === 'connector_access_denied')));
}

export function setupIsActive(status: WebModelSetupStatus | null): boolean {
  return Boolean(status && !['complete', 'cancelled', 'busy'].includes(status.stage));
}

const CONNECTOR_FAILURE_REASONS = ['connector_page_not_open', 'connector_ambiguous', 'connector_not_listed', 'connector_detail_unconfirmed', 'connector_refresh_button_missing', 'connector_refresh_rejected', 'connector_tunnel_mismatch', 'connector_create_entry_missing', 'connector_form_fields_missing', 'connector_tunnel_option_missing', 'connector_auth_option_missing', 'connector_save_button_missing', 'connector_create_rejected', 'connector_not_enabled_after_create', 'connector_not_listed_after_create'];

export function setupNeedsConnectorConsent(status: WebModelSetupStatus | null): boolean {
  return status?.stage === 'connector' && ['confirmation_required', 'connector_write_started', 'connector_unconfirmed', 'connector_refresh_unconfirmed', 'connector_access_denied', ...CONNECTOR_FAILURE_REASONS].includes(status.reason);
}

export function setupCanRetryTunnel(status: WebModelSetupStatus | null): boolean {
  return status?.stage === 'tunnel' && ['creation_started', 'creation_unconfirmed'].includes(status.reason);
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

/** 页面只能返回固定原因；显示具体操作提示，不展示任意页面诊断。 */
export function setupReasonKey(reason: string): string | null {
  switch (reason) {
    case 'login_required': return 'webModel.setup.reason.platformLogin';
    case 'page_required': return 'webModel.setup.reason.platformPage';
    case 'ambiguous_tunnel': return 'webModel.setup.reason.ambiguousTunnel';
    case 'tunnel_id_required': return 'webModel.setup.reason.tunnelId';
    case 'permission_or_page_required': return 'webModel.setup.reason.permissions';
    case 'key_generation_required': return 'webModel.setup.reason.keyReady';
    case 'project_required': return 'webModel.setup.reason.project';
    case 'key_permissions_required': return 'webModel.setup.reason.keyPermissions';
    case 'workspace_required': return 'webModel.setup.reason.workspace';
    case 'creation_started':
    case 'creation_unconfirmed': return 'webModel.setup.reason.tunnelUnconfirmed';
    case 'connector_write_started':
    case 'connector_refresh_unconfirmed':
    case 'connector_unconfirmed': return 'webModel.setup.reason.connectorUnconfirmed';
    case 'connector_access_denied': return 'webModel.setup.reason.connectorAccessDenied';
    case 'connector_ambiguous': return 'webModel.setup.reason.connectorAmbiguous';
    case 'connector_tunnel_mismatch': return 'webModel.setup.reason.connectorTunnelMismatch';
    case 'connector_refresh_rejected': return 'webModel.setup.reason.connectorRefreshRejected';
    case 'connector_page_not_open':
    case 'connector_not_listed':
    case 'connector_detail_unconfirmed':
    case 'connector_refresh_button_missing': return 'webModel.setup.reason.connectorPageNotReady';
    case 'connector_create_entry_missing':
    case 'connector_form_fields_missing':
    case 'connector_tunnel_option_missing':
    case 'connector_auth_option_missing':
    case 'connector_save_button_missing': return 'webModel.setup.reason.connectorFormChanged';
    case 'connector_create_rejected':
    case 'connector_not_enabled_after_create':
    case 'connector_not_listed_after_create': return 'webModel.setup.reason.connectorUnconfirmed';
    case 'manual_required': return 'webModel.setup.reason.manual';
    default: return null;
  }
}

/** 一个检查点只展示一条主指引，人工交接不能同时声称正在自动处理。 */
export function setupHintKey(status: WebModelSetupStatus, busy: boolean, connectionFailed = false): string {
  if (busy) return 'webModel.setup.workingHint';
  if (status.stage === 'connecting' && connectionFailed) return 'webModel.setup.reason.connectionFailed';
  if (status.stage === 'credentials') {
    if (status.reason === 'runtime_key_required') return 'webModel.setup.reason.keyPreparation';
    if (status.reason === 'login_required') return 'webModel.setup.reason.keyLogin';
    if (['key_generation_required', 'project_required', 'key_permissions_required'].includes(status.reason)) {
      return setupReasonKey(status.reason)!;
    }
  }
  if (status.stage === 'tunnel' || status.stage === 'connector') {
    if (status.stage === 'connector' && status.reason === 'confirmation_required') return 'webModel.setup.hint.confirmConnector';
    const reasonKey = setupReasonKey(status.reason);
    if (reasonKey) return reasonKey;
  }
  return `webModel.setup.hint.${status.stage}`;
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
