import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type { PluginManifest, WebModelSetupStatus } from '../shared/app-server-protocol.generated';
import { WEB_MODEL_SETUP_COMMAND_ID } from '../lib/user-message-command';
import { WEB_MODEL_PLUGIN_ID } from '../shared/web-model';

export const WEB_MODEL_SETUP_COMMAND = WEB_MODEL_SETUP_COMMAND_ID;
export const WEB_MODEL_SETUP_REQUEST = 'magi:gptWebSetup';
export type SetupAction = 'start' | 'continue' | 'cancel';

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
  return Boolean(status && ['tunnel', 'credentials', 'connecting'].includes(status.stage));
}

export function setupIsActive(status: WebModelSetupStatus | null): boolean {
  return Boolean(status && !['complete', 'cancelled', 'busy'].includes(status.stage));
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
export async function saveWebModelSetupCredentials(sessionId: string, tunnelId: string, apiKey: string): Promise<void> {
  await read(await getTransport().request(agentUrl('/api/browser/web-models/setup/credentials'), {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ sessionId, tunnelId: tunnelId.trim() || null, apiKey }),
  }));
}
