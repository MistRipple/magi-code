import { runtimeId } from '../core/identity.mjs';
import { SLOT_SURFACE } from '../runtime/client/slot-context.mjs';

// 一个槽位可以承载多个页面，身份必须包含上游 key/id，不能按槽位名去重。
export function clientViews(slots, fallbackTitle) {
  const identities = new Set();
  let settingsAssigned = false;
  return slots.map((slot) => {
    const identity = JSON.stringify([slot.name, slot.key]);
    if (identities.has(identity)) throw new Error(`上游页面身份重复：${identity}`);
    identities.add(identity);
    const settings = SLOT_SURFACE[slot.name] === 'settings';
    const primarySettings = settings && !settingsAssigned;
    if (settings) settingsAssigned = true;
    const id = primarySettings ? 'settings' : runtimeId('view', identity);
    // 会话槽位的 key 是注册身份，不能充当用户可见的插件名称。
    return { slot, id, title: slot.label ?? (slot.name.startsWith('conversation.') ? fallbackTitle : slot.key || fallbackTitle),
      entry: primarySettings ? 'ui/settings.html' : `ui/${id}.html`,
      ...(slot.priority === undefined ? {} : { priority: slot.priority }),
      placements: settings ? ['settings'] : ['conversation.input.left', 'conversation.input.right'].includes(slot.name) ? ['composerLeft']
        : slot.name === 'main' ? ['main', 'rightPane'] : ['rightPane'] };
  });
}
