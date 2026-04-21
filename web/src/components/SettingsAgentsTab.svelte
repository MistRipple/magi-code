<script lang="ts">
  import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
  import type { RoleTemplate } from '../shared/types/role-templates';
  import { isAgentBindingOperational, resolveSelectableRegistryEngines } from '../shared/model-governance';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';

  let {
    roleTemplates,
    registryAgents,
    registryEngines,
    workerConfigs,
    getAgentColor,
    getWorkerDisplayName,
    updateRoleEnabled,
    updateRoleEngine,
  } = $props<{
    roleTemplates: RoleTemplate[];
    registryAgents: AgentBinding[];
    registryEngines: ModelEngine[];
    workerConfigs: Record<string, { enabled?: boolean }>;
    getAgentColor: (templateId: string, colorToken?: string) => any;
    getWorkerDisplayName: (workerId: string) => string;
    updateRoleEnabled: (templateId: string, enabled: boolean) => void;
    updateRoleEngine: (templateId: string, engineId: string) => void;
  }>();

  const selectableEngines = $derived(resolveSelectableRegistryEngines(registryEngines, workerConfigs));

  function resolveLocalizedTemplateDisplayName(tmpl: RoleTemplate, locale: string): string {
    void locale;
    const displayNameKey = tmpl.i18n?.displayNameKey || `roleTemplate.${tmpl.templateId}.displayName`;
    const translated = i18n.t(displayNameKey);
    return translated !== displayNameKey ? translated : tmpl.displayName;
  }

  function resolveLocalizedTemplateDescription(tmpl: RoleTemplate, locale: string): string {
    void locale;
    const descriptionKey = tmpl.i18n?.descriptionKey || `roleTemplate.${tmpl.templateId}.description`;
    const translated = i18n.t(descriptionKey);
    return translated !== descriptionKey ? translated : (tmpl.description || tmpl.profile.focus.join('，'));
  }
</script>

<div class="settings-tab-inner scroll-proxy">
  <div class="agent-tiles-grid settings-scroll-panel">
    {#each roleTemplates as tmpl (tmpl.templateId)}
      {@const locale = i18n.locale}
      {@const agent = registryAgents.find((a: AgentBinding) => a.templateId === tmpl.templateId)}
      {@const isActive = agent ? agent.enabled !== false : true}
      {@const isExplicitEngine = agent?.modelSource === 'engine'}
      {@const isOperational = agent ? isAgentBindingOperational(agent, registryEngines, workerConfigs) : true}
      {@const selectValue = isExplicitEngine ? agent.engineId : ''}
      {@const agentColorPair = getAgentColor(tmpl.templateId, tmpl.defaultUI?.colorToken)}
      {@const displayName = resolveLocalizedTemplateDisplayName(tmpl, locale)}
      {@const description = resolveLocalizedTemplateDescription(tmpl, locale)}

      <div class="agent-tile" class:is-disabled={!isActive}>
        <div class="tile-head">
          <div class="brand-box">
            <div class="avatar" style="background: {isActive ? agentColorPair.muted : 'rgba(var(--foreground-rgb), 0.08)'}; color: {isActive ? agentColorPair.color : 'var(--foreground-muted)'}">
              <Icon name="bot" size={12} />
            </div>
            <div class="name-box">
              <span class="name-txt">{displayName}</span>
              {#if tmpl.templateId === 'orchestrator' || tmpl.templateId === 'commander'}
                <span class="badge-mini">CORE</span>
              {/if}
            </div>
          </div>
          <div class="switch-box">
            <Toggle checked={isActive} onchange={(v) => updateRoleEnabled(tmpl.templateId, v)} />
          </div>
        </div>

        <div class="tile-body">
          <p class="summary" title={description}>{description}</p>
        </div>

        <div class="tile-foot">
          <div class="status-box">
            {#if !isExplicitEngine}
              <div class="inherit-pill">
                <span class="ind-status-dot success"></span>
                <span class="inherit-txt">{getWorkerDisplayName('default')}</span>
              </div>
            {/if}
          </div>
          <div class="control-box">
            <div class="ind-select-wrap" class:err={isExplicitEngine && !isOperational}>
              <Icon name="model" size={10} class="icon-pre" />
              <select
                class="ind-mini-select"
                value={selectValue}
                onchange={(e) => updateRoleEngine(tmpl.templateId, (e.target as HTMLSelectElement).value)}
              >
                <option value="">{i18n.t('settings.agents.inheritOrchestrator')}</option>
                {#each selectableEngines as eng}
                  <option value={eng.id}>{getWorkerDisplayName(eng.id)}</option>
                {/each}
              </select>
              <Icon name="chevron-down" size={10} class="icon-suf" />
            </div>
          </div>
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .settings-tab-inner {
    display: flex;
    flex-direction: column;
    height: 100%;
    width: 100%;
    min-height: 0;
    overflow: hidden;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif;
    --ind-bg-card: rgba(255, 255, 255, 0.92);
    --ind-bg-card-elevated: #ffffff;
    --ind-bg-control: rgba(0, 0, 0, 0.03);
    --ind-bg-control-hover: rgba(0, 0, 0, 0.06);
    --ind-border-card: rgba(60, 60, 67, 0.16);
    --ind-border-card-strong: rgba(60, 60, 67, 0.2);
    --ind-border-control: rgba(60, 60, 67, 0.14);
    --ind-border-control-strong: rgba(60, 60, 67, 0.22);
    --ind-border-separator: rgba(60, 60, 67, 0.10);
    --ind-foreground: #1d1d1f;
    --ind-foreground-secondary: #86868b;
    --ind-foreground-muted: #86868b;
    --ind-foreground-soft: #aeaeb2;
    --ind-radius-card: 12px;
    --ind-shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.04), 0 6px 18px rgba(0, 0, 0, 0.05);
  }

  :global(body.vscode-dark) .settings-tab-inner,
  :global(body.theme-dark) .settings-tab-inner,
  :global(:root.theme-dark) .settings-tab-inner {
    --ind-bg-card: rgba(255, 255, 255, 0.04);
    --ind-bg-card-elevated: rgba(255, 255, 255, 0.07);
    --ind-bg-control: rgba(255, 255, 255, 0.05);
    --ind-bg-control-hover: rgba(255, 255, 255, 0.09);
    --ind-border-card: rgba(255, 255, 255, 0.14);
    --ind-border-card-strong: rgba(255, 255, 255, 0.20);
    --ind-border-control: rgba(255, 255, 255, 0.12);
    --ind-border-control-strong: rgba(255, 255, 255, 0.20);
    --ind-border-separator: rgba(255, 255, 255, 0.08);
    --ind-foreground: var(--foreground);
    --ind-foreground-secondary: color-mix(in srgb, var(--foreground) 55%, var(--foreground-muted) 45%);
    --ind-foreground-muted: var(--foreground-muted);
    --ind-foreground-soft: color-mix(in srgb, var(--foreground-muted) 65%, transparent);
  }

  .scroll-proxy { min-height: 0; }
  .settings-scroll-panel { flex: 1; min-height: 0; overflow-y: auto; padding: 0 4px 2px; scrollbar-width: none; }
  .settings-scroll-panel::-webkit-scrollbar { width: 0; }

  .agent-tiles-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 12px; padding-bottom: 6px; }
  .agent-tile {
    background: var(--ind-bg-card);
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    padding: 14px 18px 18px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 124px;
    box-sizing: border-box;
    transition: background 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
    box-shadow: var(--ind-shadow-sm);
    overflow: hidden;
  }
  .agent-tile:hover { border-color: var(--ind-border-card-strong); background: var(--ind-bg-card-elevated); }
  .agent-tile.is-disabled { opacity: 0.55; filter: grayscale(0.35); border-style: dashed; }

  .tile-head { display: flex; align-items: center; justify-content: space-between; min-height: 24px; flex-shrink: 0; }
  .brand-box { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .avatar { width: 22px; height: 22px; border-radius: 6px; display: flex; align-items: center; justify-content: center; flex-shrink: 0; }
  .name-box { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .name-txt { font-size: 13px; font-weight: 650; color: var(--ind-foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; letter-spacing: -0.01em; }
  .badge-mini { font-size: 8px; font-weight: 700; padding: 1px 5px; border-radius: 5px; background: color-mix(in srgb, var(--ind-bg-control) 88%, transparent); border: 1px solid var(--ind-border-separator); color: var(--ind-foreground-soft); letter-spacing: 0.04em; }
  .switch-box { transform: scale(0.84); transform-origin: right center; }

  .tile-body { min-height: 32px; display: flex; align-items: center; }
  .summary { margin: 0; font-size: 11px; color: var(--ind-foreground-muted); line-height: 1.45; display: -webkit-box; line-clamp: 2; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }

  .tile-foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: auto; min-width: 0; padding-top: 4px; }
  .status-box { flex: 1; min-width: 0; }
  .control-box { flex-shrink: 0; }
  .inherit-pill { display: inline-flex; align-items: center; gap: 6px; min-height: 22px; max-width: 140px; padding: 0; border-radius: 999px; background: transparent; border: none; box-sizing: border-box; }
  .inherit-txt { font-size: 10px; font-weight: 600; color: var(--ind-foreground-soft); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

  .ind-select-wrap { width: 138px; max-width: 100%; min-width: 0; height: 28px; background: rgba(0, 0, 0, 0.01); border: 1px solid var(--ind-border-control); border-radius: 8px; display: flex; align-items: center; padding: 0 8px; gap: 6px; transition: background 0.18s ease, border-color 0.18s ease, color 0.18s ease; box-sizing: border-box; }
  .ind-select-wrap:hover { background: rgba(0, 0, 0, 0.03); border-color: var(--ind-border-control-strong); }

  :global(body.vscode-dark) .ind-select-wrap,
  :global(body.theme-dark) .ind-select-wrap,
  :global(:root.theme-dark) .ind-select-wrap { background: rgba(255, 255, 255, 0.03); }
  :global(body.vscode-dark) .ind-select-wrap:hover,
  :global(body.theme-dark) .ind-select-wrap:hover,
  :global(:root.theme-dark) .ind-select-wrap:hover { background: rgba(255, 255, 255, 0.06); }
  .ind-select-wrap.err { border-color: color-mix(in srgb, var(--error) 30%, var(--ind-border-control)); background: color-mix(in srgb, var(--error) 8%, var(--ind-bg-control)); }
  :global(.icon-pre), :global(.icon-suf) { opacity: 0.56; flex-shrink: 0; }
  :global(.icon-suf) { margin-left: auto; }
  .ind-mini-select { flex: 1; min-width: 0; background: transparent; border: none; font-size: 10.5px; font-weight: 500; color: var(--ind-foreground-secondary); outline: none; cursor: pointer; padding: 0; appearance: none; }
  .ind-mini-select option { color: var(--foreground); background: var(--background); }
  .ind-select-wrap:hover .ind-mini-select { color: var(--ind-foreground); }
  .ind-select-wrap:hover :global(.icon-pre), .ind-select-wrap:hover :global(.icon-suf) { opacity: 0.78; }

  .ind-status-dot { width: 4.5px; height: 4.5px; border-radius: 999px; flex-shrink: 0; }
  .ind-status-dot.success { background: #34c759; box-shadow: 0 0 6px rgba(52, 199, 89, 0.55); }

  @media (max-width: 580px) {
    .agent-tiles-grid { grid-template-columns: 1fr; }
    .tile-foot { align-items: flex-start; flex-direction: column; }
    .control-box, .ind-select-wrap { width: 100%; }
  }
</style>
