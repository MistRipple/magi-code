<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import FileName from '../components/FileName.svelte';
  import FileTypeIcon from '../components/FileTypeIcon.svelte';
  import Disclosure from '../components/Disclosure.svelte';
  import type { IconName } from '../lib/icons';
  import { resolveToolIcon } from '../lib/tool-icons';
  import PreviewStatus from './PreviewStatus.svelte';
  import { previewFiles, type DemoCard, type DemoStatus } from './workbench-fixtures';

  let { card, compact = false, onpreview, onfeedback }: {
    card: DemoCard;
    compact?: boolean;
    onpreview: (id: string, view?: string) => void;
    onfeedback: (text: string) => void;
  } = $props();
  let expanded = $state(untrack(() => card.expanded ?? false));
  let manual = $state(false);
  let localStatus = $state<DemoStatus | 'cancelled' | undefined>(undefined);
  let actionResult = $state('');
  let choice = $state(0);
  let submitted = $state(false);
  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  const instanceId = $props.id();
  const controlId = `demo-card-${instanceId}`;
  const status = $derived(localStatus ?? card.status);
  const persistent = $derived(['approval', 'question'].includes(card.kind) || status === 'error');
  const open = $derived(persistent || (manual ? expanded : compact ? card.kind === 'code' : expanded));
  const summary = $derived(submitted || localStatus === 'success' ? actionResult : card.summary);
  const icon = $derived<IconName>(({ thought: 'brain', plan: 'note', steps: 'list', search: resolveToolIcon('search_text'), terminal: resolveToolIcon('shell_exec'), tool: resolveToolIcon('web_fetch'), mcp: resolveToolIcon('mcp__demo__query'), diff: 'git-branch', code: 'code', agents: 'bot', approval: 'shield', question: 'question', diagram: 'layers' } as const)[card.kind]);
  const files = $derived(previewFiles.filter(file => card.files?.includes(file.id)));
  const isCode = $derived(card.kind === 'code');

  function toggle() { manual = true; expanded = !open; }
  function act(label: string) {
    if (card.kind === 'approval') {
      if (submitted) return;
      submitted = true;
      localStatus = label.includes('暂不') ? 'cancelled' : 'success';
      actionResult = label.includes('暂不') ? '已暂缓发布，文档草稿已保留。' : '已记录本次授权，演示文档已加入发布队列。';
      onfeedback('演示反馈：' + actionResult);
    } else if (card.kind === 'question') {
      submitted = true;
      localStatus = 'success';
      actionResult = '已选择：' + card.items?.[choice]?.title;
      onfeedback('演示反馈：' + actionResult);
    } else if (label === '重试') {
      if (localStatus === 'running') return;
      localStatus = 'running';
      actionResult = '正在重新读取模板…';
      retryTimer = setTimeout(() => { localStatus = 'success'; actionResult = '模板已读取，可继续准备发布。'; onfeedback('演示重试完成'); }, 1700);
    } else onpreview(card.files?.[0] || 'export-guide', card.kind === 'diagram' ? 'document' : 'diff');
  }
  async function copy() {
    try {
      await navigator.clipboard.writeText(card.code || card.body || '');
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => copied = false, 2500);
    } catch { onfeedback('浏览器未允许复制，请检查剪贴板权限。'); }
  }
  function choose(event: KeyboardEvent, index: number) {
    if (submitted || !['ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const length = card.items?.length || 0;
    if (!length) return;
    choice = event.key === 'Home' ? 0 : event.key === 'End' ? length - 1 : (index + (['ArrowDown', 'ArrowRight'].includes(event.key) ? 1 : -1) + length) % length;
    const group = (event.currentTarget as HTMLElement).closest('[role="radiogroup"]');
    group?.querySelectorAll<HTMLButtonElement>('[role="radio"]')[choice]?.focus();
  }
  onDestroy(() => { clearTimeout(copyTimer); clearTimeout(retryTimer); });
</script>

<section class="card" class:is-open={open} class:code-card={isCode} class:approval={card.kind === 'approval'} class:error-card={status === 'error'} data-demo-kind={card.kind}>
  {#snippet headerContent()}
    <span class="card-icon" aria-hidden="true"><Icon name={icon} size={14} /></span>
    <span class="card-title" title={card.title}>{card.title}</span>
    {#if summary}<span class="card-summary" title={summary}>{summary}</span>{/if}
  {/snippet}
  <div class="card-head wb-card-header-row">
    {#if persistent}
      <div class="wb-card-toggle wb-card-static">{@render headerContent()}</div>
    {:else}
      <button class="wb-card-toggle" onclick={toggle} aria-expanded={open} aria-controls={controlId}>{@render headerContent()}</button>
    {/if}
    <div class="meta wb-card-meta">
      {#if card.meta}<span class="duration">{card.meta.replaceAll(' · ', '　')}</span>{/if}
      {#if isCode}<button class="icon-btn wb-copy" class:is-copied={copied} onclick={copy} aria-label={copied ? '已复制' : '复制代码'} title={copied ? '已复制' : '复制代码'}><span aria-hidden="true"><Icon name={copied ? 'check' : 'copy'} size={14}/></span></button>{/if}
      {#if status && !isCode}<span class="wb-card-status-slot"><PreviewStatus {status} /></span>{/if}
      {#if !persistent}
        <span class="wb-card-chevron-slot"><button class="wb-card-chevron" onclick={toggle} aria-label={open ? `折叠${card.title}` : `展开${card.title}`} aria-expanded={open} aria-controls={controlId}><span aria-hidden="true"><Icon name={open ? 'chevron-down' : 'chevron-right'} size={12}/></span></button></span>
      {/if}
    </div>
  </div>
  <div id={controlId}>
    <Disclosure expanded={open}>
      {#if isCode}
        <div class="code-surface">{#each (card.code || '').split('\n') as line, index}<div class="code-line"><span class="ln" aria-hidden="true">{index + 1}</span><code>{line || ' '}</code></div>{/each}</div>
      {:else}
        <div class="card-content" class:divider={!['diff','terminal'].includes(card.kind)}>
          {#if card.body && !['terminal','question','approval'].includes(card.kind)}<p class="wb-card-description" class:muted={card.kind === 'thought'}>{actionResult && status === 'success' ? '模板读取已恢复，文档内容可以继续准备。' : card.body}</p>{/if}
          {#if card.kind === 'diff'}
            <div class="file-list">{#each files as file}<button class="file-row wb-file-button" onclick={() => onpreview(file.id, 'diff')} title={file.path}><span aria-hidden="true"><FileTypeIcon path={file.path} size={15}/></span><FileName name={file.name}/><span class="file-stats"><span class="green">+{file.added}</span>{#if file.removed}<span class="red">−{file.removed}</span>{/if}</span><span class="wb-file-open" aria-hidden="true"><Icon name="external-link" size={12}/></span></button>{/each}</div>
          {:else if card.kind === 'terminal' || card.code}
            <pre class="inset wb-terminal-output" class:muted={status === 'running'}>{actionResult && status === 'success' ? '$ request template\nHTTP 200\nTemplate loaded.' : card.code}</pre>
          {/if}
          {#if card.kind === 'diagram'}
            <div class="wb-flow" role="img" aria-label="从会话正文生成 Markdown，再保存为本地文件">{#each card.items || [] as item, i}<div class="wb-flow-node"><Icon name={i === 0 ? 'chat' : i === 1 ? 'code' : 'download'} size={18}/><span>{item.title}</span></div>{#if i < (card.items?.length || 0) - 1}<span class="dim" aria-hidden="true"><Icon name="arrow-right" size={14}/></span>{/if}{/each}</div>
          {:else if card.kind === 'question'}
            <p class="wb-card-description">{card.body}</p><div role="radiogroup" aria-label="发布目录">{#each card.items || [] as item, index}<button class="choice" class:wb-choice-selected={choice === index} role="radio" aria-checked={choice === index} tabindex={choice === index ? 0 : -1} disabled={submitted} onclick={() => choice = index} onkeydown={(event) => choose(event, index)}><span class="radio" aria-hidden="true"></span><span>{item.title}<span class="wb-choice-detail">{item.detail}</span></span>{#if index === 0}<span class="badge">推荐</span>{/if}</button>{/each}</div>
          {:else if card.kind === 'approval'}
            <p class="wb-card-description">{card.body}</p>{#each files as file}<button class="wb-inline-file" onclick={() => onpreview(file.id,'document')}><FileTypeIcon path={file.path} size={14}/><FileName name={file.name}/></button>{/each}
          {:else if card.items}
            <ul class="wb-card-items">{#each card.items as item}<li>{#if item.status}<PreviewStatus status={item.status}/>{:else}<span aria-hidden="true" class="dim"><Icon name={card.kind === 'agents' ? 'bot' : 'file-text'} size={14}/></span>{/if}<div class="wb-item-copy"><span>{item.title}</span>{#if item.detail}<span class="wb-item-detail">{item.detail}</span>{/if}</div>{#if item.meta}<span class="dim wb-item-meta">{item.meta}</span>{/if}</li>{/each}</ul>
          {/if}
          {#if actionResult}<p class="wb-action-result" role="status">{actionResult}</p>{/if}
          {#if card.actions?.length && !isCode}<div class="actions">{#each card.actions as action,i}<button class:primary={['approval','question'].includes(card.kind) && i === 0} class:text-btn={!['approval','question'].includes(card.kind) || i !== 0} disabled={submitted || (action === '重试' && status === 'running')} onclick={() => act(action)}>{action}</button>{/each}</div>{/if}
        </div>
      {/if}
    </Disclosure>
  </div>
</section>

<style>
  .wb-card-header-row { display: flex; align-items: center; min-width: 0; }
  .wb-card-toggle { display: flex; align-items: center; gap: 8px; padding: 0; width: 100%; flex: 1; min-width: 0; text-align: left; align-self: stretch; }
  .wb-card-toggle:not(.wb-card-static):hover { background: var(--hover); }
  .wb-card-toggle:focus-visible { outline: 0; box-shadow: inset 0 0 0 2px var(--accent); border-radius: 9px; }
  .wb-card-static { cursor: default; }
  .wb-copy { margin: 0; flex: none; }
  .wb-card-meta { align-self: stretch; align-items: center; white-space: nowrap; }
  .wb-card-status-slot, .wb-card-chevron-slot { display: inline-flex; align-items: center; justify-content: center; flex: 0 0 14px; width: 14px; height: 26px; }
  .wb-card-chevron-slot { flex-basis: 26px; width: 26px; }
  .wb-card-chevron { display: inline-flex; align-items: center; justify-content: center; flex: none; width: 26px; height: 26px; padding: 0; border-radius: 5px; }
  .wb-card-chevron > span { display: flex; align-items: center; }
  .wb-card-chevron:hover { background: var(--hover); }
  .wb-copy.is-copied { color: var(--success, #10b981); }
  .wb-copy.is-copied > span { animation: demo-copy-pop 450ms var(--magi-spring-bouncy, ease-out), demo-copy-fade 250ms 2250ms forwards; }
  @keyframes demo-copy-pop { 50% { transform: scale(1.12); } }
  @keyframes demo-copy-fade { to { opacity: 0; } }
  @media (prefers-reduced-motion: reduce) { .wb-copy.is-copied > span { animation: none; } }
  .wb-card-description { line-height: 1.8; text-wrap: pretty; margin: 0 0 12px; }
  .wb-card-description:last-child { margin-bottom: 0; }
  .wb-terminal-output { white-space: pre; font-size: 12px; line-height: 1.85; }
  .wb-card-items { list-style: none; margin: 0; padding: 0; display: grid; gap: 12px; }
  .wb-card-items li { display: flex; gap: 9px; align-items: center; }
  .wb-item-copy { display: grid; min-width: 0; flex: 1; }
  .wb-item-detail, .wb-choice-detail { display: block; color: var(--muted); font-size: 12px; }
  .wb-item-meta { margin-left: auto; font-size: 12px; white-space: nowrap; }
  .wb-file-button { width: 100%; min-width: 0; text-align: left; border-radius: 5px; }
  .wb-file-button:hover, .wb-inline-file:hover { background: var(--hover); }
  .wb-file-open { color: var(--dim); opacity: 0; }
  .wb-file-button:hover .wb-file-open, .wb-file-button:focus-visible .wb-file-open { opacity: 1; }
  .wb-inline-file { display: flex; max-width: 100%; align-items: center; gap: 7px; padding: 6px 0; border-radius: 5px; color: var(--muted); }
  .wb-flow { display: flex; align-items: center; justify-content: center; gap: 13px; padding: 7px 0; }
  .wb-flow-node { display: flex; align-items: center; justify-content: center; gap: 8px; min-width: 0; border-radius: 7px; padding: 14px 12px; background: var(--code); box-shadow: inset 0 0 0 var(--hairline) var(--line); font-size: 12px; }
  .wb-action-result { margin: 12px 0 0; font-size: 12px; color: var(--muted); text-wrap: pretty; }
  .wb-choice-selected { box-shadow: inset 0 0 0 var(--hairline) var(--accent); background: color-mix(in srgb, var(--accent) 6%, transparent); }
  .wb-choice-selected .radio { box-shadow: inset 0 0 0 4px var(--accent); }
  button:disabled { cursor: default; opacity: .55; }
  @media (max-width: 700px) {
    .wb-flow { gap: 7px; flex-wrap: wrap; }
    .wb-flow-node { padding: 10px 8px; }
    .wb-item-meta { display: none; }
  }
</style>
