import { i18n } from '../stores/i18n.svelte';
import { addToast } from '../stores/messages.svelte';
import { updateAgentRuntimeSetting } from '../web/agent-api';
import type { LocaleCode } from '../i18n/locales';

/**
 * 切换界面语言的唯一入口（侧栏语言菜单与外观设置共用）。
 * 先由 daemon 持久化，成功后 `updateAgentRuntimeSetting` 才切换界面语言，
 * 依赖语言的数据（如角色模板）随后按新语言重载。返回是否切换成功。
 */
export async function switchLocale(code: LocaleCode): Promise<boolean> {
  if (code === i18n.locale) return true;
  try {
    await updateAgentRuntimeSetting('locale', code);
    return true;
  } catch (error) {
    addToast('error', error instanceof Error ? error.message : String(error));
    return false;
  }
}
