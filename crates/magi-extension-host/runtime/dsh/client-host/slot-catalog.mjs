// Canonical DSH 0.2.2-rc.3 client UI slot catalog.
// Declares upstream slot contracts so mounted slots render consistently and unsupported ones report cleanly.

export const PHASE1_SETTINGS_SLOTS = new Set([
  'settings.section',
  'settings.section.icon',
  'settings.plugins.tab',
  'plugins.tab',
  'plugins.bundle.config',
  'plugins.row.config',
  'plugins.item',
  'plugins.detail.actions',
  'plugins.detail.badge',
  'plugins.detail.section',
  'plugins.add.actions',
  'plugins.bundle.activation',
]);

export const PHASE1_MOUNTED_SLOTS = new Set([
  ...PHASE1_SETTINGS_SLOTS,
  'main',
  'sidebar.right.pane.tab.title',
  'sidebar.right.tab.document',
  'sidebar.right.tab.document.action',
]);

export const ROOT_CHILD_SLOT_SPECS = Object.freeze({
  'contextMenu.content': { kind: 'keyed', scope: 'root' },
  'contextMenu.icon': { kind: 'keyed', scope: 'root' },
  'conversation.approval.detail': { kind: 'single', scope: 'session' },
  'conversation.chat.assistant-actions': { kind: 'list', scope: 'session' },
  'conversation.chat.commandview': { kind: 'keyed', scope: 'session' },
  'conversation.chat.flow': { kind: 'single', scope: 'session' },
  'conversation.chat.markdown.fence': { kind: 'hoc', scope: 'session' },
  'conversation.chat.message.images': { kind: 'single', scope: 'session' },
  'conversation.chat.node': { kind: 'keyed', scope: 'session' },
  'conversation.chat.reasoning.body': { kind: 'single', scope: 'session' },
  'conversation.chat.text-interaction': { kind: 'keyed', scope: 'session' },
  'conversation.chat.turnTail': { kind: 'list', scope: 'session' },
  'conversation.chat.user.before': { kind: 'chain', scope: 'session' },
  'conversation.chat.user.images': { kind: 'single', scope: 'session' },
  'conversation.composer': { kind: 'chain', scope: 'session' },
  'conversation.composer.bar': { kind: 'single', scope: 'session-maybe' },
  'conversation.composer.dock': { kind: 'list', scope: 'session' },
  'conversation.header': { kind: 'single', scope: 'session-maybe' },
  'conversation.header.leading': { kind: 'single', scope: 'root' },
  'conversation.hero.agentPreset': { kind: 'single', scope: 'session-maybe' },
  'conversation.hero.brand.mark': { kind: 'single', scope: 'root' },
  'conversation.hero.frame': { kind: 'hoc', scope: 'root' },
  'conversation.hero.options': { kind: 'list', scope: 'session' },
  'conversation.hero.workspace': { kind: 'single', scope: 'root' },
  'conversation.hero.workspace.directoryFlow': { kind: 'single', scope: 'root' },
  'conversation.input.activity': { kind: 'single', scope: 'session' },
  'conversation.input.attachments': { kind: 'single', scope: 'session-maybe' },
  'conversation.input.dock': { kind: 'list', scope: 'session' },
  'conversation.input.left': { kind: 'list', scope: 'session' },
  'conversation.input.model': { kind: 'single', scope: 'session' },
  'conversation.input.model.label': { kind: 'single', scope: 'session' },
  'conversation.input.model.options': { kind: 'list', scope: 'session' },
  'conversation.input.model.pane': { kind: 'keyed', scope: 'session' },
  'conversation.input.overlay': { kind: 'list', scope: 'session' },
  'conversation.input.permission': { kind: 'single', scope: 'session' },
  'conversation.input.plan': { kind: 'single', scope: 'session' },
  'conversation.input.right': { kind: 'list', scope: 'session' },
  'conversation.plan-review.actions': { kind: 'list', scope: 'session' },
  'conversation.session': { kind: 'single', scope: 'session' },
  'conversation.session.header': { kind: 'single', scope: 'session' },
  'conversation.session.header.actions': { kind: 'list', scope: 'session' },
  'conversation.session.header.actions.subagent.footer': { kind: 'list', scope: 'session' },
  'conversation.session.header.corner': { kind: 'single', scope: 'session' },
  'conversation.session.header.lineage': { kind: 'single', scope: 'session' },
  'conversation.session.header.utilities': { kind: 'list', scope: 'session' },
  'conversation.session.header.views.actions': { kind: 'list', scope: 'root' },
  'conversation.trajectory.images': { kind: 'single', scope: 'session' },
  'conversation.user.text.node': { kind: 'keyed', scope: 'session' },
  'conversation.view': { kind: 'list', scope: 'session' },
  'deliverables.file.actions': { kind: 'list', scope: 'session' },
  'deliverables.review.file.actions': { kind: 'list', scope: 'session' },
  'forge.change-request.card.actions': { kind: 'list', scope: 'session' },
  'forge.changes.file.menu': { kind: 'list', scope: 'session' },
  'forge.changes.toolbar': { kind: 'list', scope: 'session' },
  'forge.diff.comment.actions': { kind: 'list', scope: 'session' },
  'forge.diff.line.annotation': { kind: 'keyed', scope: 'session' },
  'main': { kind: 'keyed', scope: 'root' },
  'main.conversation': { kind: 'single', scope: 'session-maybe' },
  'plugins.add.actions': { kind: 'list', scope: 'root' },
  'plugins.bundle.activation': { kind: 'keyed', scope: 'root' },
  'plugins.bundle.config': { kind: 'keyed', scope: 'root' },
  'plugins.detail.actions': { kind: 'list', scope: 'root' },
  'plugins.detail.badge': { kind: 'list', scope: 'root' },
  'plugins.detail.section': { kind: 'list', scope: 'root' },
  'plugins.item': { kind: 'list', scope: 'root' },
  'plugins.row.config': { kind: 'keyed', scope: 'root' },
  'plugins.tab': { kind: 'list', scope: 'root' },
  'rightbar': { kind: 'single', scope: 'root' },
  'rightbar.session': { kind: 'single', scope: 'session' },
  'settings.action': { kind: 'list', scope: 'root' },
  'settings.close': { kind: 'single', scope: 'root' },
  'settings.general.item': { kind: 'list', scope: 'root' },
  'settings.header': { kind: 'single', scope: 'root' },
  'settings.launcher': { kind: 'single', scope: 'root' },
  'settings.launcher.notifications': { kind: 'single', scope: 'root' },
  'settings.models.footer': { kind: 'list', scope: 'root' },
  'settings.models.provider-card': { kind: 'keyed', scope: 'root' },
  'settings.models.sign-in': { kind: 'single', scope: 'root' },
  'settings.onboarding': { kind: 'list', scope: 'root' },
  'settings.plugins.tab': { kind: 'list', scope: 'root' },
  'settings.section': { kind: 'list', scope: 'root' },
  'settings.section.icon': { kind: 'list', scope: 'root', inline: true },
  'settings.trigger': { kind: 'single', scope: 'root' },
  'shell.bottom': { kind: 'single', scope: 'root' },
  'shell.leading': { kind: 'single', scope: 'root' },
  'shell.overlay': { kind: 'list', scope: 'root' },
  'shell.quota-notice': { kind: 'chain', scope: 'root' },
  'sidebar': { kind: 'single', scope: 'root' },
  'sidebar.brand.mark': { kind: 'single', scope: 'root' },
  'sidebar.brand.name': { kind: 'single', scope: 'root' },
  'sidebar.chat.conversation': { kind: 'single', scope: 'session' },
  'sidebar.footer.action': { kind: 'list', scope: 'root' },
  'sidebar.panellist': { kind: 'list', scope: 'root' },
  'sidebar.panellist.actions': { kind: 'list', scope: 'root' },
  'sidebar.right.pane.tab': {
    kind: 'keyed',
    scope: 'session',
    inject: { hooks: { tabInfo: (_standard, useTabInfo) => useTabInfo } },
  },
  'sidebar.right.pane.tab.title': {
    kind: 'keyed',
    scope: 'session',
    inject: { hooks: { tabInfo: (_standard, useTabInfo) => useTabInfo } },
  },
  'sidebar.right.tab.document': { kind: 'keyed', scope: 'session' },
  'sidebar.right.tab.document.action': { kind: 'keyed', scope: 'session' },
  'sidebar.right.tab.document.actions': { kind: 'list', scope: 'session' },
  'sidebar.right.tab.document.office.pdf': { kind: 'keyed', scope: 'session' },
  'sidebar.right.tab.document.unpreviewable': { kind: 'list', scope: 'session' },
  'sidebar.right.tab.files.actions': { kind: 'list', scope: 'session' },
  'sidebar.right.tab.guide': { kind: 'chain', scope: 'session' },
  'sidebar.right.tab.guide.actions': { kind: 'list', scope: 'root' },
  'sidebar.right.tab.guide.entry': { kind: 'keyed', scope: 'session' },
  'sidebar.right.tab.menu.item': { kind: 'list', scope: 'session' },
  'sidebar.session.action': { kind: 'hoc', scope: 'root' },
  'sidebar.session.row.hover': { kind: 'list', scope: 'root' },
  'sidebar.session.row.leading': { kind: 'list', scope: 'root' },
  'sidebar.session.row.trailing': { kind: 'list', scope: 'root' },
  'sidebar.settings': { kind: 'single', scope: 'root' },
  'sidebar.toggle.badge': { kind: 'single', scope: 'root' },
  'sidebar.workspaces': { kind: 'single', scope: 'root' },
  'sidebar.workspaces.directoryFlow': { kind: 'single', scope: 'root' },
  'sidebar.workspaces.session.menu.item': { kind: 'list', scope: 'root' },
  'sidebar.workspaces.session.row.action': { kind: 'list', scope: 'root' },
  'tool.call.images': { kind: 'single', scope: 'session' },
  'tool.call.toolview': { kind: 'keyed', scope: 'session' },
  'tool.view.cordis': { kind: 'keyed', scope: 'session' },
  'workspace.picker.directoryFlow': { kind: 'single', scope: 'root' },
  'workspace.toast': { kind: 'keyed', scope: 'root' },
});

export function summarizePluginSlots(slots, pluginId, rightPaneTabs) {
  const mountedSlots = [];
  const unmountedSlots = [];
  let hasSettingsView = false;
  const mainKeys = [];

  for (const name of Object.keys(ROOT_CHILD_SLOT_SPECS)) {
    const entries = slots.entries(name);
    if (!entries || entries.length === 0) continue;
    if (name === 'sidebar.right.pane.tab' && rightPaneTabs.length > 0) {
      mountedSlots.push(name);
    } else if (PHASE1_MOUNTED_SLOTS.has(name)) {
      if (name === 'plugins.bundle.config' || name === 'plugins.bundle.activation') {
        if (entries.some(e => e.options?.key === pluginId || !pluginId)) {
          mountedSlots.push(name);
          hasSettingsView = true;
        }
        continue;
      }
      if (name === 'plugins.row.config') {
        if (entries.some(e => !pluginId || e.options?.key === pluginId || e.options?.key?.startsWith(`${pluginId}#`))) {
          mountedSlots.push(name);
          hasSettingsView = true;
        }
        continue;
      }
      mountedSlots.push(name);
      if (name === 'main') {
        for (const e of entries) {
          if (typeof e.options?.key === 'string') mainKeys.push(e.options.key);
        }
      } else if (
        name !== 'settings.section.icon'
        && !name.startsWith('sidebar.right.')
      ) {
        hasSettingsView = true;
      }
    } else {
      unmountedSlots.push(name);
    }
  }

  return {
    mountedSlots: mountedSlots.sort(),
    unmountedSlots: unmountedSlots.sort(),
    hasSettingsView,
    mainKeys,
  };
}
