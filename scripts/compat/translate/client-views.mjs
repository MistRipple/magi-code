import { runtimeId } from '../core/identity.mjs';

const SETTINGS_SLOTS = new Set(['settings.section', 'plugins.bundle.config', 'settings.plugins.tab', 'plugins.tab']);

// 一个槽位可以承载多个页面，身份必须包含上游 key/id，不能按槽位名去重。
export function clientViews(slots, fallbackTitle) {
  const identities = new Set();
  let settingsAssigned = false;
  return slots.map((slot) => {
    const identity = JSON.stringify([slot.name, slot.key]);
    if (identities.has(identity)) throw new Error(`上游页面身份重复：${identity}`);
    identities.add(identity);
    const settings = SETTINGS_SLOTS.has(slot.name);
    const primarySettings = settings && !settingsAssigned;
    if (settings) settingsAssigned = true;
    const id = primarySettings ? 'settings' : runtimeId('view', identity);
    return { slot, id, title: slot.label ?? (slot.key || fallbackTitle),
      entry: primarySettings ? 'ui/settings.html' : `ui/${id}.html`,
      placements: settings ? ['settings'] : slot.name === 'main' ? ['main', 'rightPane'] : ['rightPane'] };
  });
}
