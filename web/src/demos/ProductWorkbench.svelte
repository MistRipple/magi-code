<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import FileName from '../components/FileName.svelte';
  import FileTypeIcon from '../components/FileTypeIcon.svelte';
  import { beginPanelResize } from '../lib/panel-resize';
  import WorkbenchCard from './WorkbenchCard.svelte';
  import PreviewStatus from './PreviewStatus.svelte';
  import WorkbenchSettings from './WorkbenchSettings.svelte';
  import { initializeAppearanceRuntime, getAppearanceRuntimeSnapshot, previewAppearanceTheme, restoreActiveAppearance } from '../appearance/runtime';
  import {
    demoNotice,
    previewFiles,
    workbenchConversation,
    workbenchRuntime,
    workbenchSessions,
    type DemoMessage,
  } from './workbench-fixtures';

  type Scene = 'conversation' | 'delivery' | 'process' | 'approval';
  type PreviewTab = 'document' | 'diff' | 'terminal' | 'agent';

  let scene = $state<Scene>('conversation');
  let page = $state<'chat' | 'agents' | 'mcp'>('chat');
  let skinId = $state('');
  let skins = $state<{ id: string; name: string }[]>([]);
  let themeBusy = $state(false);
  let selectedSession = $state('markdown-export');
  let searchVisible = $state(false);
  let search = $state('');
  let newConversation = $state(false);
  let model = $state('默认模型');
  let compact = $state(false);
  let attachment = $state('');
  let attachmentInput = $state<HTMLInputElement>(null!);
  let activePreview = $state<PreviewTab>('document');
  let previewFileId = $state('export-guide');
  let previewWidth = $state(40);
  let previewClosed = $state(false);
  let sidebarClosed = $state(false);
  let sidebarMobileOpen = $state(false);
  let mobilePreview = $state(false);
  let windowWidth = $state(1440);
  let draft = $state('');
  let followLatest = $state(true);
  let bottomDistance = $state(0);
  let composerHeight = $state(150);
  let composing = false;
  let compositionEnded = -Infinity;
  let extraMessages = $state<DemoMessage[]>([]);
  let streamText = $state('');
  let streaming = $state(false);
  let streamTimer: ReturnType<typeof setInterval> | undefined;
  let pathCopied = $state(false);
  let feedback = $state('');
  let feedbackTimer: ReturnType<typeof setTimeout> | undefined;
  let pathTimer: ReturnType<typeof setTimeout> | undefined;
  let scrollEl = $state<HTMLElement>(null!);
  let resizeCleanup: (() => void) | undefined;

  const selectedFile = $derived(previewFiles.find((file) => file.id === previewFileId) ?? previewFiles[0]);
  const currentSession = $derived(workbenchSessions.find((session) => session.id === selectedSession) ?? workbenchSessions[0]);
  const sessionExample = $derived<DemoMessage[]>(selectedSession === 'markdown-export' ? [] : [
    { id: `${selectedSession}-request`, role: 'user', author: '你', time: currentSession.time, text: currentSession.title },
    { id: `${selectedSession}-reply`, role: 'assistant', author: 'Magi', time: currentSession.time, text: currentSession.preview,
      cards: workbenchConversation.flatMap((message) => message.cards ?? []).filter((card) =>
        selectedSession === 'mcp-library' ? card.kind === 'search'
        : selectedSession === 'ui-review' ? card.kind === 'agents'
        : selectedSession === 'release-notes' ? card.kind === 'diff'
        : card.id === 'failure') },
  ]);
  const scenarioMessages = $derived(scene === 'conversation'
    ? workbenchConversation
    : scene === 'delivery'
      ? workbenchConversation.filter((message) => ['request', 'changes', 'answer'].includes(message.id))
      : scene === 'process'
        ? workbenchConversation.filter((message) => ['request', 'investigation', 'changes'].includes(message.id))
        : workbenchConversation.filter((message) => ['followup', 'sync'].includes(message.id)));
  const visibleMessages = $derived([...(newConversation ? [] : selectedSession !== 'markdown-export' ? sessionExample : scenarioMessages), ...extraMessages]);

  function notify(text: string) {
    feedback = text;
    clearTimeout(feedbackTimer);
    feedbackTimer = setTimeout(() => feedback = '', 2600);
  }

  function openPreview(id: string, view = 'document') {
    previewFileId = id;
    activePreview = (view === 'diff' ? 'diff' : view === 'terminal' ? 'terminal' : 'document') as PreviewTab;
    previewClosed = false;
    mobilePreview = window.matchMedia('(max-width:1080px)').matches;
  }

  function setScene(next: Scene) {
    scene = next; page = 'chat'; newConversation = false; selectedSession = 'markdown-export';
    void tick().then(() => { scrollEl?.scrollTo({top:0}); onScroll(); });
    if (next === 'approval') activePreview = 'agent';
    if (next === 'delivery') activePreview = 'document';
    if (next === 'process') activePreview = 'terminal';
    mobilePreview = false;
  }

  function onScroll() {
    if (!scrollEl) return;
    bottomDistance = scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight;
    followLatest = bottomDistance <= 40;
  }

  function jumpLatest() {
    scrollEl?.scrollTo({ top: scrollEl.scrollHeight, behavior: 'smooth' });
    followLatest = true;
  }

  async function submit() {
    if (!draft.trim() || streaming) return;
    extraMessages = [...extraMessages, { id: `local-${Date.now()}`, role: 'user', author: '你', time: '现在', text: draft.trim() }];
    draft = '';
    attachment = '';
    streaming = true;
    streamText = '';
    await tick();
    scrollEl.scrollTop = scrollEl.scrollHeight;
    followLatest = true;
    const answer = Array.from('这是本地流式演示。正文逐字轻柔显现，工具、文件和代理继续沿用同一套卡片规范。你可以向上滚动阅读，输出不会把视图拉回。');
    let offset = 0;
    streamTimer = setInterval(async () => {
      streamText += answer[offset++] ?? '';
      await tick();
      if (followLatest) scrollEl.scrollTop = scrollEl.scrollHeight;
      bottomDistance = scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight;
      if (offset >= answer.length) {
        clearInterval(streamTimer);
        extraMessages = [...extraMessages, { id: `reply-${Date.now()}`, role: 'assistant', author: 'Magi', time: '现在', text: streamText }];
        streamText = ''; streaming = false;
      }
    }, 30);
  }

  function inputKey(event: KeyboardEvent) {
    if (event.key !== 'Enter' || event.shiftKey) return;
    if (composing || event.isComposing || event.keyCode === 229) return;
    if (performance.now() - compositionEnded < 500) { event.preventDefault(); compositionEnded = -Infinity; return; }
    event.preventDefault(); void submit();
  }

  async function changeSkin(id: string) {
    const pack = getAppearanceRuntimeSnapshot().library?.themes.find(({ pack }) => pack.id === id)?.pack;
    if (!pack || themeBusy) return;
    themeBusy = true;
    try { await previewAppearanceTheme(pack); skinId = id; }
    catch { notify('皮肤加载失败，请重试'); }
    finally { themeBusy = false; }
  }

  async function restoreSkin() {
    if (themeBusy) return;
    themeBusy = true;
    try { await restoreActiveAppearance(); skinId = getAppearanceRuntimeSnapshot().library?.activeThemeId ?? ''; }
    catch { notify('恢复皮肤失败，请重试'); }
    finally { themeBusy = false; }
  }

  async function copyPath() {
    try { await navigator.clipboard.writeText(selectedFile.path); pathCopied = true; notify('已复制文件路径'); clearTimeout(pathTimer); pathTimer = setTimeout(() => pathCopied = false, 2500); }
    catch { notify('浏览器未允许复制文件路径'); }
  }

  function resizeStart(event: PointerEvent) {
    const shell = event.currentTarget instanceof HTMLElement ? event.currentTarget.closest('.wb-app') as HTMLElement | null : null;
    const workspace = event.currentTarget instanceof HTMLElement ? event.currentTarget.parentElement : null;
    if (!workspace || !shell) return;
    const start = previewWidth;
    const wasClosed = previewClosed;
    let collapse = false;
    resizeCleanup?.();
    shell.classList.add('resizing');
    resizeCleanup = beginPanelResize(event, {
      move: (next) => {
        const rect = workspace.getBoundingClientRect();
        collapse = (next.clientX - rect.left) / rect.width >= .82;
        const divider = 6;
        const left = Math.max(220, Math.min(rect.width - divider - 280, next.clientX - rect.left));
        const percent = (left / (rect.width - divider)) * 100;
        previewWidth = Math.max(24, Math.min(64, 100 - percent));
      },
      finish: (cancelled) => {
        shell.classList.remove('resizing');
        if (cancelled) { previewWidth = start; previewClosed = wasClosed; }
        else if (collapse) { previewClosed = true; mobilePreview = false; }
        resizeCleanup = undefined;
      },
    });
  }

  function resetSplit() { previewWidth = 50; previewClosed = false; }

  function togglePreview() {
    if (windowWidth <= 1080) { previewClosed = false; mobilePreview = !mobilePreview; }
    else { previewClosed = !previewClosed; mobilePreview = false; }
  }

  function startConversation() {
    clearInterval(streamTimer); streaming = false; streamText = '';
    page = 'chat'; extraMessages = []; newConversation = true; sidebarMobileOpen = false;
    draft = ''; attachment = '';
    bottomDistance = 0; followLatest = true;
    void tick().then(onScroll);
  }

  function navigateTabs(event: KeyboardEvent) {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    const tabs = Array.from((event.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>('[role=tab]'));
    const index = tabs.indexOf(event.target as HTMLButtonElement);
    if (index < 0) return;
    event.preventDefault();
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : (index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
    tabs[next].focus(); tabs[next].click();
  }

  function splitKey(event: KeyboardEvent) {
    if (event.key === 'Home') { event.preventDefault(); resetSplit(); }
    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      event.preventDefault(); previewWidth = Math.max(24, Math.min(64, previewWidth + (event.key === 'ArrowLeft' ? 3 : -3)));
    }
  }

  onMount(() => {
    void tick().then(onScroll);
    void initializeAppearanceRuntime().then(() => {
      const library = getAppearanceRuntimeSnapshot().library;
      skins = library?.themes.map(({ pack }) => ({ id: pack.id, name: pack.name })) ?? [];
      skinId = getAppearanceRuntimeSnapshot().activeTheme?.id ?? library?.activeThemeId ?? '';
    }).catch(() => notify('皮肤读取失败，显示中性预览'));
    return () => { clearTimeout(feedbackTimer); clearTimeout(pathTimer); clearInterval(streamTimer); resizeCleanup?.(); };
  });
</script>

<svelte:head><title>Magi · 工作台设计预览</title></svelte:head>
<svelte:window bind:innerWidth={windowWidth} />

<div class="wb-app" class:sidebar-closed={sidebarClosed} class:sidebar-mobile-open={sidebarMobileOpen}>
  <aside class="wb-sidebar" aria-label="工作区导航">
    <div class="wb-brand"><Icon name="sparkles" size={22} /><span>Magi</span><button class="wb-icon-button" onclick={() => { sidebarClosed = true; sidebarMobileOpen = false; }} aria-label="收起侧栏"><Icon name="sidebar-toggle" size={16} /></button></div>
    <div class="wb-project"><Icon name="folder" size={15} /><span>magi-rust-rewrite</span><Icon name="chevron-down" size={12} /></div>
    <button class="wb-new" onclick={startConversation}><Icon name="plus" size={15} />新建会话</button>
    <nav class="wb-nav" aria-label="主导航">
      <button class="wb-nav-item" class:active={page === 'chat'} onclick={() => { page = 'chat'; sidebarMobileOpen = false; }}><Icon name="chat" size={15} />会话<span class="wb-session-meta">{workbenchSessions.length}</span></button>
      <button class="wb-nav-item" class:active={page === 'agents'} onclick={() => { page = 'agents'; sidebarMobileOpen = false; }}><Icon name="bot" size={15} />代理</button>
      <button class="wb-nav-item" class:active={page === 'mcp'} onclick={() => { page = 'mcp'; sidebarMobileOpen = false; }}><Icon name="plug" size={15} />MCP 配置</button>
    </nav>
    <div class="wb-group-label"><span>最近会话</span><button class="wb-icon-button" onclick={() => searchVisible = !searchVisible} aria-label="搜索会话"><Icon name="search" size={14} /></button></div>
    <div class="wb-session-list">{#if searchVisible}<input class="wb-input" aria-label="搜索会话" placeholder="搜索会话…" bind:value={search} />{/if}
      {#each ['今天', '昨天'] as group}
        <div class="wb-group-label wb-session-group">{group}</div>
        {#each workbenchSessions.filter((session) => session.group === group && session.title.toLowerCase().includes(search.toLowerCase())) as session}
          <button class="wb-session" class:selected={session.id === selectedSession} onclick={() => { selectedSession = session.id; page = 'chat'; newConversation = false; extraMessages = []; sidebarMobileOpen = false; }}>
            {#if session.status}<PreviewStatus status={session.status} />{/if}<span>{session.title}</span><span class="wb-session-meta">{session.time}</span>
          </button>
        {/each}
      {/each}
    </div>
    <div class="wb-sidebar-footer"><span>演示环境</span><span class="status green" role="img" aria-label="服务正常" title="服务正常"></span><button class="wb-text-button" onclick={() => notify('预览不会写入真实配置')}>说明</button></div>
  </aside>

  <main class="wb-main">
    <header class="wb-topbar">
      <button class="wb-icon-button wb-mobile-sidebar" onclick={() => sidebarMobileOpen = !sidebarMobileOpen} aria-label="打开导航"><Icon name="sidebar-toggle" size={16} /></button>
      {#if sidebarClosed}<button class="wb-icon-button" onclick={() => sidebarClosed = false} aria-label="展开侧栏"><Icon name="sidebar-toggle" size={16} /></button>{/if}
      <span class="wb-breadcrumb">工作区 / {page === 'chat' ? '会话' : page === 'agents' ? '代理' : 'MCP'}</span><span class="wb-title">{page === 'mcp' ? 'MCP 服务' : page === 'agents' ? '代理' : newConversation ? '新会话' : currentSession.title}</span>
      <div class="wb-top-actions">{#if page === 'chat' && !newConversation && selectedSession === 'markdown-export'}<span class="wb-pill"><PreviewStatus status={scene === 'delivery' ? 'success' : scene === 'process' ? 'running' : 'pending'} />{scene === 'delivery' ? '导出功能已完成' : scene === 'process' ? '查看执行过程' : workbenchRuntime.summary}</span>{/if}<select id="skin-select" class="skin-select" aria-label="预览皮肤" value={skinId} disabled={themeBusy || !skins.length} onchange={(event) => changeSkin(event.currentTarget.value)}>{#if !skins.length}<option>加载当前皮肤…</option>{/if}{#each skins as skin}<option value={skin.id}>{skin.name}</option>{/each}</select><button class="wb-text-button" class:active={compact} onclick={() => compact = !compact} aria-pressed={compact}>精简过程</button><button class="wb-text-button" disabled={themeBusy} onclick={restoreSkin}>当前皮肤</button><button class="wb-icon-button" class:active={mobilePreview} onclick={togglePreview} aria-label={windowWidth <= 1080 ? (mobilePreview ? '返回会话' : '打开预览') : (previewClosed ? '打开预览' : '收起预览')} title={windowWidth <= 1080 ? (mobilePreview ? '返回会话' : '打开预览') : (previewClosed ? '打开预览' : '收起预览')}><Icon name={mobilePreview ? 'chat' : 'sidebar-toggle'} size={16} /></button></div>
    </header>
    <div class="wb-demo-bar"><span>最终工作台 Demo</span><small>{demoNotice}</small>{#if page === 'chat'}<div class="wb-scene" role="tablist" tabindex="-1" aria-label="演示场景" onkeydown={navigateTabs}>
      <button class:active={scene === 'conversation'} onclick={() => setScene('conversation')} id="scene-conversation" role="tab" aria-controls="scene-panel" tabindex={scene === 'conversation' ? 0 : -1} aria-selected={scene === 'conversation'}>完整会话</button><button class:active={scene === 'delivery'} onclick={() => setScene('delivery')} id="scene-delivery" role="tab" aria-controls="scene-panel" tabindex={scene === 'delivery' ? 0 : -1} aria-selected={scene === 'delivery'}>交付结果</button><button class:active={scene === 'process'} onclick={() => setScene('process')} id="scene-process" role="tab" aria-controls="scene-panel" tabindex={scene === 'process' ? 0 : -1} aria-selected={scene === 'process'}>执行过程</button><button class:active={scene === 'approval'} onclick={() => setScene('approval')} id="scene-approval" role="tab" aria-controls="scene-panel" tabindex={scene === 'approval' ? 0 : -1} aria-selected={scene === 'approval'}>等待确认</button>
    </div>{/if}<a href="/src/demos/icon-library.html">图标库</a><a href="/src/demos/message-cards.html">查看卡片清单</a></div>

    {#if page !== 'chat'}<WorkbenchSettings {page} onback={() => page = 'chat'} onfeedback={notify} />{:else}
    <div id="scene-panel" role="tabpanel" aria-labelledby={`scene-${scene}`} class="wb-workspace" class:preview-closed={previewClosed} class:mobile-preview={mobilePreview} style={`grid-template-columns:minmax(0, ${100 - previewWidth}fr) 6px minmax(280px, ${previewWidth}fr)`}>
      <section class="wb-conversation" aria-label="会话内容" style={`--wb-composer-height:${composerHeight}px`}>
        <div class="wb-chat-scroll" role="region" aria-label="消息历史" bind:this={scrollEl} onscroll={onScroll} onwheel={() => followLatest = false} ontouchmove={() => followLatest = false}>
          <div class="wb-messages">{#if newConversation && !extraMessages.length}<div class="wb-welcome"><Icon name="sparkles" size={26}/><h1>从一个想法开始。</h1><p>在这里体验 Magi 的消息、工具与流式输出。</p><button class="wb-control" onclick={() => draft = '帮我梳理这个项目的产品化细节'}>梳理项目细节 <Icon name="plus" size={13}/></button></div>{/if}
            {#each visibleMessages as message (message.id)}
              {#if message.role === 'user'}
                <article class="wb-user"><div class="wb-message-author"><strong>{message.author}</strong><time>{message.time}</time></div><p class="wb-user-bubble">{message.text}</p></article>
              {:else}
                <article class="wb-assistant"><div class="wb-message-author"><Icon name="sparkles" size={17} /><strong>{message.author}</strong><time>{message.time}</time></div><div class="wb-answer">{#if message.text}<p class="prose">{message.text}</p>{/if}{#each message.cards ?? [] as card (card.id)}<WorkbenchCard {card} {compact} onpreview={openPreview} onfeedback={notify} />{/each}</div></article>
              {/if}
            {/each}
            {#if streaming}<article class="wb-assistant"><div class="wb-message-author"><Icon name="sparkles" size={17} /><strong>Magi</strong><PreviewStatus status="running" /></div><p class="prose wb-stream" aria-label="Magi 正在回复">{#each Array.from(streamText) as char, i (i)}<span class="wb-ink">{char}</span>{/each}</p></article>{/if}
          </div>
        </div>
        {#if bottomDistance > 120 && !followLatest}<button class="wb-jump" onclick={jumpLatest}>跳至最新 <Icon name="chevron-down" size={12} /></button>{/if}
        <div class="wb-composer-wrap" bind:clientHeight={composerHeight}><div class="wb-composer">{#if attachment}<div class="wb-attachment"><FileTypeIcon path={attachment} size={14}/><FileName name={attachment}/><button class="wb-icon-button" aria-label="移除附件" onclick={() => attachment = ''}><Icon name="x" size={12}/></button></div>{/if}<textarea bind:value={draft} placeholder="继续告诉 Magi 要做什么…" aria-label="消息输入" oncompositionstart={() => composing = true} oncompositionend={() => { composing = false; compositionEnded = performance.now(); }} onkeydown={inputKey}></textarea><div class="wb-composer-tools"><select class="wb-model" aria-label="演示模型" bind:value={model}><option>默认模型</option><option>快速模型</option><option>深度推理</option></select><span class="wb-context"><Icon name="folder" size={12} />当前工作区</span><input type="file" hidden bind:this={attachmentInput} onchange={(event) => { attachment = event.currentTarget.files?.[0]?.name ?? ''; event.currentTarget.value = ''; }} /><button class="wb-icon-button" onclick={() => attachmentInput.click()} aria-label="添加附件"><Icon name="plus" size={15} /></button><button class="wb-send" disabled={streaming || !draft.trim()} onclick={submit} aria-label="发送"><Icon name="send" size={15} /></button></div></div><div class="wb-footnote"><span>Enter 发送 · Shift + Enter 换行</span><span>演示数据，不会执行真实操作</span></div></div>
      </section>
      <button class="wb-divider" onkeydown={splitKey} ondblclick={resetSplit} onpointerdown={resizeStart} aria-label="调整预览宽度，双击恢复均分" title="拖动调整，双击恢复 50/50"></button>
      <aside class="wb-preview" aria-label="预览与详情">
        <div class="wb-preview-tabs" role="tablist" tabindex="-1" aria-label="预览内容" onkeydown={navigateTabs}><button class="wb-tab" class:active={activePreview === 'document'} onclick={() => activePreview = 'document'} id="preview-document" role="tab" aria-controls="preview-panel" tabindex={activePreview === 'document' ? 0 : -1} aria-selected={activePreview === 'document'}><FileTypeIcon path={selectedFile.name} size={14} /><FileName name={selectedFile.name} /></button><button class="wb-tab" class:active={activePreview === 'diff'} onclick={() => activePreview = 'diff'} id="preview-diff" role="tab" aria-controls="preview-panel" tabindex={activePreview === 'diff' ? 0 : -1} aria-selected={activePreview === 'diff'}><Icon name="git-branch" size={14} />Diff</button><button class="wb-tab" class:active={activePreview === 'terminal'} onclick={() => activePreview = 'terminal'} id="preview-terminal" role="tab" aria-controls="preview-panel" tabindex={activePreview === 'terminal' ? 0 : -1} aria-selected={activePreview === 'terminal'}><Icon name="terminal" size={14} />终端</button><button class="wb-tab" class:active={activePreview === 'agent'} onclick={() => activePreview = 'agent'} id="preview-agent" role="tab" aria-controls="preview-panel" tabindex={activePreview === 'agent' ? 0 : -1} aria-selected={activePreview === 'agent'}><Icon name="bot" size={14} />代理</button><button class="wb-icon-button wb-preview-close" onclick={() => { previewClosed = true; mobilePreview = false; }} aria-label="关闭预览"><Icon name="x" size={14} /></button></div>
        <div id="preview-panel" class="wb-preview-panel" role="tabpanel" aria-labelledby={`preview-${activePreview}`}>
        {#if activePreview === 'agent'}
          <div class="wb-panel-page"><header><div><h1>代理运行</h1><p class="muted">本轮协作状态</p></div><span class="wb-pill"><PreviewStatus status="pending" />等待确认</span></header><h2>协作回执</h2>{#each workbenchRuntime.agents as agent}<div class="wb-card-row"><PreviewStatus status={agent.status} /><div><strong>{agent.title}</strong><p>{agent.detail}</p></div></div>{/each}<h2 class="wb-section-title">工具状态</h2><div class="wb-card-row"><PreviewStatus status="running" /><div><strong>团队知识库</strong><p>读取发布目录</p></div></div><div class="wb-card-row"><PreviewStatus status="error" /><div><strong>发布模板</strong><p>连接超时，可重试</p></div></div></div>
        {:else}
          <div class="wb-preview-toolbar"><span><FileName name={selectedFile.path} /></span><span>{selectedFile.language}</span><button class="wb-icon-button" onclick={copyPath} aria-label="复制路径"><Icon name={pathCopied ? 'check' : 'copy'} size={13} /></button></div>
          <div class="wb-preview-body">
            {#if activePreview === 'document' && selectedFile.language === 'markdown'}<article class="wb-doc"><h1>导出会话</h1><p>在会话右上角打开「更多」，选择「导出 Markdown」。浏览器会保存一个 <code>.md</code> 文件，文件名来自当前会话标题。</p><h2>保留的内容</h2><ul><li>你的消息与 Magi 的回答</li><li>代码块、列表、链接与表格</li><li>消息的原始顺序</li></ul><h2>数据范围</h2><p>导出使用当前会话中已加载的消息，不会上传到额外的服务。</p></article>
            {:else if activePreview === 'diff' || activePreview === 'document'}<div class="wb-source" class:wb-diff={activePreview === 'diff'}>{#if activePreview === 'diff' && selectedFile.removed}<div class="wb-code-row wb-diff-del"><span>1</span><span class="wb-diff-mark" aria-hidden="true">−</span><code># 旧版导出说明</code></div><div class="wb-code-row wb-diff-del"><span>2</span><span class="wb-diff-mark" aria-hidden="true">−</span><code>手动复制内容到文件。</code></div>{/if}{#each selectedFile.content.split('\n') as line, i}<div class="wb-code-row" class:wb-diff-add={activePreview === 'diff'}><span>{i + 1}</span>{#if activePreview === 'diff'}<span class="wb-diff-mark" aria-hidden="true">+</span>{/if}<code>{line || ' '}</code></div>{/each}</div>
            {:else}<pre class="wb-source wb-terminal">$ npm run check && npm run test:export

svelte-check found 0 errors and 0 warnings

✓ 保留 Markdown 代码块
✓ 正确处理中文文件名
✓ Tests 4 passed (1)</pre>{/if}
          </div><footer class="wb-preview-footer"><span>{selectedFile.added} 行新增</span><span>{selectedFile.removed} 行删除</span><span class="spacer"></span><span>只读预览</span></footer>
        {/if}
        </div>
      </aside>
    </div>
    {/if}
  </main>
  {#if feedback}<div class="wb-toast" role="status">{feedback}</div>{/if}
</div>
