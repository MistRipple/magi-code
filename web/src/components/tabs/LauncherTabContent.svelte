<script module lang="ts">
  /** 本会话正在做事的代理；由 RightPane 从代理运行投影挑选，这里只负责展示。 */
  export interface LauncherAgent {
    agentRunId: string;
    label: string;
    goal: string;
    statusLabel: string;
    queued: boolean;
    color: string;
  }

  export interface LauncherTool {
    key: string;
    kind: string;
    label: string;
    detail?: string;
    icon: 'terminal' | 'globe' | 'plug';
    enabled: boolean;
    disabledReason: string;
    pluginId?: string;
    contributionId?: string;
  }
</script>

<script lang="ts">
  /**
   * 右栏新标签页：先选「做什么」，再变成对应的面板。
   *
   * 纯展示组件：能力判断（终端、浏览器、插件视图）和实际创建都留在 RightPane，
   * 这里只渲染它给出的工具列表和最近项，并把选择回传。没有自己的业务状态。
   */
  import Icon from '../Icon.svelte';
  import FileTypeIcon from '../FileTypeIcon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { formatRelativeTime } from '../../lib/relative-time';
  import type { RecentPaneItem } from '../../stores/right-pane-recent.svelte';

  interface Props {
    tools: readonly LauncherTool[];
    agents: readonly LauncherAgent[];
    recent: readonly RecentPaneItem[];
    /** 网页只能由 Desktop 的内置浏览器打开；不可用时最近的网页项置灰并说明原因。 */
    webAvailable: boolean;
    webUnavailableReason: string;
    busy?: boolean;
    onChooseTool: (tool: LauncherTool) => void;
    onOpenAgent: (agent: LauncherAgent) => void;
    onOpenRecent: (item: RecentPaneItem) => void;
  }

  let { tools, agents, recent, webAvailable, webUnavailableReason, busy = false, onChooseTool, onOpenAgent, onOpenRecent }: Props = $props();

  const now = Date.now();

  function recentSubtitle(item: RecentPaneItem): string {
    if (item.kind === 'web') {
      try {
        return new URL(item.url).host;
      } catch {
        return i18n.t('rightPane.launcher.recentWeb');
      }
    }
    return item.displayPath ?? item.filepath;
  }
</script>

<div class="launcher" data-testid="right-pane-launcher">
  <section class="launcher-section" aria-labelledby="launcher-tools-title">
    <h2 class="launcher-title" id="launcher-tools-title">{i18n.t('rightPane.launcher.tools')}</h2>
    <div class="launcher-tools">
      {#each tools as tool (tool.key)}
        <button
          type="button"
          class="launcher-tool"
          class:launcher-tool--disabled={!tool.enabled}
          data-kind={tool.kind}
          data-tool-key={tool.key}
          data-plugin-id={tool.pluginId}
          data-contribution-id={tool.contributionId}
          aria-disabled={!tool.enabled || busy}
          title={tool.enabled ? undefined : tool.disabledReason}
          onclick={() => { if (tool.enabled && !busy) onChooseTool(tool); }}
        >
          <span class="launcher-tool-icon" aria-hidden="true"><Icon name={tool.icon} size={16} /></span>
          <span class="launcher-tool-copy">
            <span class="launcher-tool-label">{tool.label}</span>
            {#if tool.enabled && tool.detail}
              <span class="launcher-tool-detail">{tool.detail}</span>
            {:else if !tool.enabled}
              <span class="launcher-tool-detail">{tool.disabledReason}</span>
            {/if}
          </span>
        </button>
      {/each}
    </div>
  </section>

  {#if agents.length > 0}
    <section class="launcher-section" aria-labelledby="launcher-agents-title">
      <h2 class="launcher-title" id="launcher-agents-title">{i18n.t('rightPane.launcher.agents')}</h2>
      <ul class="launcher-recent">
        {#each agents as agent (agent.agentRunId)}
          <li>
            <button
              type="button"
              class="launcher-recent-item"
              data-agent-run-id={agent.agentRunId}
              aria-disabled={busy}
              title={agent.goal || agent.label}
              onclick={() => { if (!busy) onOpenAgent(agent); }}
            >
              <span class="launcher-recent-icon launcher-agent-icon" style={`--agent-color: ${agent.color};`} aria-hidden="true">
                <Icon name={agent.queued ? 'taskPending' : 'loader'} size={14} class={agent.queued ? '' : 'spinning'} />
              </span>
              <span class="launcher-recent-copy">
                <span class="launcher-recent-label">{agent.label}</span>
                {#if agent.goal}<span class="launcher-recent-sub">{agent.goal}</span>{/if}
              </span>
              <span class="launcher-recent-time">{agent.statusLabel}</span>
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if recent.length > 0}
    <section class="launcher-section" aria-labelledby="launcher-recent-title">
      <h2 class="launcher-title" id="launcher-recent-title">{i18n.t('rightPane.launcher.recent')}</h2>
      <ul class="launcher-recent">
        {#each recent as item (item.key)}
          {@const unavailable = item.kind === 'web' && !webAvailable}
          <li>
            <button
              type="button"
              class="launcher-recent-item"
              class:launcher-tool--disabled={unavailable}
              data-recent-kind={item.kind}
              aria-disabled={unavailable || busy}
              title={unavailable ? webUnavailableReason : recentSubtitle(item)}
              onclick={() => { if (!unavailable && !busy) onOpenRecent(item); }}
            >
              <span class="launcher-recent-icon" aria-hidden="true">
                {#if item.kind === 'code'}
                  <FileTypeIcon path={item.displayPath ?? item.filepath} size={16} />
                {:else}
                  <Icon name="globe" size={16} />
                {/if}
              </span>
              <span class="launcher-recent-copy">
                <span class="launcher-recent-label">{item.label}</span>
                <span class="launcher-recent-sub">{recentSubtitle(item)}</span>
              </span>
              <span class="launcher-recent-time">{formatRelativeTime(item.at, now, i18n.locale)}</span>
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</div>

<style>
  .launcher {
    display: flex;
    flex-direction: column;
    gap: var(--space-6);
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    overflow-y: auto;
    padding: var(--space-6) var(--space-5);
  }

  .launcher-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    width: 100%;
    max-width: 720px;
    margin: 0 auto;
  }

  .launcher-title {
    margin: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .launcher-tools {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: var(--space-3);
  }

  /* 工具与最近项都是真实按钮：整块都是点击热区，状态用背景而不是改变尺寸。 */
  .launcher-tool,
  .launcher-recent-item {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    box-sizing: border-box;
    width: 100%;
    min-height: 44px;
    padding: var(--space-3) var(--space-4);
    border: none;
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    color: var(--foreground);
    font: inherit;
    text-align: start;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .launcher-tool:hover:not(.launcher-tool--disabled),
  .launcher-recent-item:hover:not(.launcher-tool--disabled) {
    background: var(--surface-hover);
  }

  .launcher-tool:focus-visible,
  .launcher-recent-item:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--primary);
  }

  .launcher-tool--disabled {
    cursor: not-allowed;
    color: var(--foreground-muted);
  }

  .launcher-tool-icon,
  .launcher-recent-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 20px;
    color: var(--foreground-muted);
  }

  .launcher-agent-icon {
    color: var(--agent-color, var(--foreground-muted));
  }

  .launcher-tool-copy,
  .launcher-recent-copy {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
  }

  .launcher-tool-label,
  .launcher-recent-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .launcher-tool-detail,
  .launcher-recent-sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .launcher-recent {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .launcher-recent-time {
    flex: 0 0 auto;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  @media (hover: none), (pointer: coarse) {
    .launcher-tool,
    .launcher-recent-item {
      min-height: 48px;
    }
  }
</style>
