<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import ContextUsageRing from '../components/ContextUsageRing.svelte';
  import SelectField from '../components/SelectField.svelte';
  import {
    applyAppearanceSnapshot, getAppearanceRuntimeSnapshot, initializeAppearanceRuntime,
    previewAppearanceTheme, restoreActiveAppearance,
  } from '../appearance/runtime';
  import type { AppearanceSnapshot } from '../appearance/contract';

  type Scope = 'all' | 'main' | 'worker' | 'auxiliary';
  type Scene = 'completed' | 'running' | 'unobserved' | 'zero' | 'empty';
  type Layout = 'desktop' | 'split' | 'phone';
  const scopes: { value: Scope; label: string }[] = [
    { value: 'all', label: '全部调用' }, { value: 'main', label: '主对话' },
    { value: 'worker', label: '子代理' }, { value: 'auxiliary', label: '辅助调用' },
  ];
  const layouts: { value: Layout; label: string }[] = [
    { value: 'desktop', label: '桌面宽屏' }, { value: 'split', label: '窄分栏' }, { value: 'phone', label: '手机' },
  ];
  const scenes: { value: Scene; label: string }[] = [
    { value: 'completed', label: '已完成' }, { value: 'running', label: '生成中' },
    { value: 'unobserved', label: '缓存未观测' }, { value: 'zero', label: '明确零命中' }, { value: 'empty', label: '新会话' },
  ];
  // 固定示例，仅用于视觉与交互评审。性能数值不是真实模型测量。
  const examples = {
    main: { calls: 61, observed: 61, hits: 51, input: 1152504, read: 880896, output: 18240, timed: 24, ttftSum: 91.2, textTokens: 12960, generationSeconds: 288 },
    worker: { calls: 9, observed: 9, hits: 1, input: 107038, read: 7424, output: 3180, timed: 6, ttftSum: 36, textTokens: 1440, generationSeconds: 48 },
    auxiliary: { calls: 60, observed: 60, hits: 0, input: 13148, read: 0, output: 5160, timed: 0, ttftSum: 0, textTokens: 0, generationSeconds: 0 },
  };
  let layout = $state<Layout>('desktop');
  let scene = $state<Scene>('completed');
  let scope = $state<Scope>('all');
  let open = $state(true);
  let expanded = $state<'tokens' | 'performance' | null>(null);
  let contextInstance = $state(0);
  let draft = $state('');
  let feedback = $state('');
  let feedbackTimer: ReturnType<typeof setTimeout>;
  let statsButton = $state<HTMLButtonElement>();
  let chatColumn = $state<HTMLElement>();
  let skins = $state<{ value: string; label: string }[]>([]);
  let skinId = $state('');
  let skinBusy = $state(false);
  let themeStatus = $state('跟随 Magi 当前皮肤');
  let previewTick = $state(0);
  let simTimer: ReturnType<typeof setTimeout>;
  let composing = false;
  let compositionEnded = -Infinity;

  const stats = $derived.by(() => {
    const parts = scope === 'all' ? Object.values(examples) : [examples[scope]];
    const sum = (key: keyof typeof examples.main) => parts.reduce((value, item) => value + item[key], 0);
    const calls = scene === 'empty' ? 0 : sum('calls');
    const observed = scene === 'empty' || scene === 'unobserved' ? 0 : sum('observed');
    const input = calls ? sum('input') : 0;
    const read = scene === 'zero' || !observed ? 0 : sum('read');
    const hits = scene === 'zero' || !observed ? 0 : sum('hits');
    const timed = calls ? sum('timed') : 0;
    return {
      calls, observed, input, read, hits, timed, output: calls ? sum('output') : 0,
      rate: observed && input ? read / input : null,
      requestRate: observed ? hits / observed : null,
      ttft: timed ? sum('ttftSum') / timed : null,
      tps: timed ? sum('textTokens') / sum('generationSeconds') : null,
    };
  });
  const running = $derived(scene === 'running');
  const canSend = $derived(draft.trim().length > 0);
  const scopeLabel = $derived(scopes.find(item => item.value === scope)?.label ?? '全部调用');
  const liveSpeed = $derived((43 + Math.sin(previewTick * .6) * 2).toFixed(1));
  const compact = (value: number) => value >= 1e6 ? `${(value / 1e6).toFixed(2)}M` : value >= 1000 ? `${(value / 1000).toFixed(1)}K` : `${value}`;
  const exact = (value: number) => value.toLocaleString('zh-CN');
  const percent = (value: number | null) => value == null ? '未提供' : `${(value * 100).toFixed(1)}%`;

  function notify(message: string) {
    feedback = message;
    clearTimeout(feedbackTimer);
    feedbackTimer = setTimeout(() => feedback = '', 2600);
  }
  function closeStats(returnFocus = false) {
    open = false;
    if (returnFocus) statsButton?.focus();
  }
  function toggleStats() {
    open = !open;
    if (open) contextInstance++;
  }
  function placeStats(node: HTMLElement) {
    let frame = 0;
    const measure = () => {
      if (!statsButton || !chatColumn) return;
      const trigger = statsButton.getBoundingClientRect();
      const parent = chatColumn.getBoundingClientRect();
      const vv = window.visualViewport;
      const minLeft = Math.max(parent.left + 12, (vv?.offsetLeft ?? 0) + 12);
      const maxRight = Math.min(parent.right - 12, (vv?.offsetLeft ?? 0) + (vv?.width ?? window.innerWidth) - 12);
      const width = Math.min(356, maxRight - minLeft);
      const minTop = Math.max(parent.top + 8, (vv?.offsetTop ?? 0) + 10);
      const available = Math.max(80, trigger.top - minTop - 10);
      node.style.width = `${width}px`;
      node.style.maxHeight = `${Math.min(560, available)}px`;
      node.style.left = `${Math.max(minLeft, Math.min(trigger.right - width, maxRight - width))}px`;
      node.style.top = `${Math.max(minTop, trigger.top - node.getBoundingClientRect().height - 10)}px`;
    };
    const schedule = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(measure); };
    const observer = new ResizeObserver(schedule);
    observer.observe(node);
    if (chatColumn) observer.observe(chatColumn);
    window.addEventListener('resize', schedule);
    window.addEventListener('scroll', schedule, true);
    window.visualViewport?.addEventListener('resize', schedule);
    window.visualViewport?.addEventListener('scroll', schedule);
    schedule();
    return { destroy() {
      observer.disconnect(); cancelAnimationFrame(frame);
      window.removeEventListener('resize', schedule);
      window.removeEventListener('scroll', schedule, true);
      window.visualViewport?.removeEventListener('resize', schedule);
      window.visualViewport?.removeEventListener('scroll', schedule);
    } };
  }
  function submit() {
    if (!canSend && running) { scene = 'completed'; notify('演示输出已停止'); return; }
    if (!canSend) return;
    draft = ''; scene = 'running';
    notify('已提交本地演示，不调用真实模型');
    clearTimeout(simTimer);
    simTimer = setTimeout(() => { scene = 'completed'; notify('演示输出已完成'); }, 5000);
  }
  function inputKey(event: KeyboardEvent) {
    if (event.key !== 'Enter' || event.shiftKey) return;
    if (composing || event.isComposing || event.keyCode === 229) return;
    if (performance.now() - compositionEnded < 500) { event.preventDefault(); return; }
    event.preventDefault(); submit();
  }
  async function changeSkin(id: string) {
    const theme = getAppearanceRuntimeSnapshot().library?.themes.find(({ pack }) => pack.id === id)?.pack;
    if (!theme || skinBusy) return;
    skinBusy = true;
    try { await previewAppearanceTheme(theme); skinId = id; themeStatus = `临时预览：${theme.name}`; }
    catch { notify('皮肤预览失败'); }
    finally { skinBusy = false; }
  }
  async function restoreSkin() {
    await restoreActiveAppearance();
    skinId = getAppearanceRuntimeSnapshot().library?.activeThemeId ?? '';
    themeStatus = '跟随 Magi 当前皮肤';
  }
  $effect(() => {
    if (!running) return;
    const timer = setInterval(() => previewTick++, 600);
    return () => clearInterval(timer);
  });
  onMount(() => {
    const clickAway = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (!target.closest('.session-stats-popover, .session-stats-trigger, .review-header')) closeStats();
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && open) { event.preventDefault(); closeStats(true); }
    };
    // 原上下文按钮会停止 click 冒泡，捕获阶段统一关闭互斥浮层。
    document.addEventListener('click', clickAway, true);
    document.addEventListener('keydown', escape);
    void (async () => {
      try {
        const embedded = document.getElementById('demo-appearance-snapshot')?.textContent;
        if (embedded) await applyAppearanceSnapshot(JSON.parse(embedded) as AppearanceSnapshot);
        else await initializeAppearanceRuntime();
        const library = getAppearanceRuntimeSnapshot().library;
        skins = library?.themes.map(({ pack }) => ({ value: pack.id, label: pack.name })) ?? [];
        skinId = library?.activeThemeId ?? '';
      } catch { themeStatus = '离线预览 · Magi 默认配色'; }
      await tick();
    })();
    return () => {
      document.removeEventListener('click', clickAway, true); document.removeEventListener('keydown', escape);
      clearTimeout(feedbackTimer); clearTimeout(simTimer);
    };
  });
</script>

<div class="stats-demo">
  <header class="review-header">
    <div class="review-title"><span class="review-logo"><Icon name="sparkles" size={19} /></span><div><h1>输入区 · 会话统计</h1><p>Magi 交互 Demo <span>／ 示例数据</span></p></div></div>
    <div class="review-controls">
      <label class="review-field"><span>布局</span><SelectField label="预览布局" bind:value={layout} options={layouts} /></label>
      <label class="review-field"><span>状态</span><SelectField label="演示状态" bind:value={scene} options={scenes} /></label>
      <label class="review-field skin-field"><span>皮肤</span><SelectField label="预览皮肤" value={skinId} options={skins.length ? skins : [{ value: '', label: '当前皮肤' }]} disabled={skinBusy || !skins.length} onchange={changeSkin} /></label>
      <button class="review-icon" title="恢复当前皮肤" aria-label="恢复当前皮肤" onclick={restoreSkin}><Icon name="refresh" size={16} /></button>
    </div>
  </header>

  <main class="preview-area">
    <div class="workbench layout-{layout}">
      <aside class="app-sidebar" aria-label="工作台导航示意">
        <div class="sidebar-brand"><Icon name="sparkles" size={22} /><strong>Magi</strong><Icon name="sidebar-toggle" size={15} /></div>
        <button class="sidebar-new" onclick={() => { scene = 'empty'; scope = 'all'; }} title="新会话演示"><Icon name="plus" size={16} /><span>新建会话</span></button>
        <div class="sidebar-caption">项目</div>
        <div class="sidebar-project"><Icon name="grid" size={14} /><span>magi-rust-rewrite</span><Icon name="chevron-down" size={12} /></div>
        <button class="sidebar-session selected" onclick={() => scene = 'completed'}><span class="selected-dot"></span><span>输入区统计与缓存观察</span></button>
        <div class="sidebar-session"><Icon name="chat" size={13} /><span>会话运行时架构收敛</span></div>
        <div class="sidebar-session"><Icon name="chat" size={13} /><span>工具调用记录整理</span></div>
        <div class="sidebar-bottom"><Icon name="settings" size={16} /><span>设置</span><span class="sidebar-version">3.0.61</span></div>
      </aside>
      <section class="chat-column" bind:this={chatColumn} aria-label="Magi 会话演示">
        <header class="chat-header"><div class="chat-title"><Icon name="chat" size={15} /><span>输入区统计与缓存观察</span></div><span class="session-status"><i class:running></i>{running ? '正在输出' : scene === 'empty' ? '新会话' : '已完成'}</span><button class="review-icon" aria-label="会话更多操作" onclick={() => notify('这是会话统计视觉演示')}><Icon name="more-horizontal" size={17} /></button></header>
        <div class="chat-body">
          {#if scene === 'empty'}
            <div class="empty-conversation"><Icon name="sparkles" size={26} /><h2>从一个想法开始</h2><p>发送消息后，这里会积累本会话的性能与用量。</p></div>
          {:else}
            <div class="date-label">当前会话</div>
            <div class="user-message">把模型性能和缓存使用情况放进输入区，电脑和手机都要能方便查看。</div>
            <article class="assistant-message">
              <div class="assistant-by"><Icon name="sparkles" size={18} /><strong>Magi</strong><span>主对话</span></div>
              <p>我把统计放在了输入区已有控件旁边。上下文圆环继续显示窗口占用，新增入口负责会话的输出性能与累计用量。</p>
              <div class="process-summary"><Icon name="chevron-right" size={13} /><span>查看输入区与上下文组件</span><span class="process-meta">2 个文件</span><i class="success-dot"></i></div>
              <div class="file-line"><Icon name="file-code" size={14} /><span>InputArea.svelte</span><span>ContextUsageRing.svelte</span></div>
              <p>展开后可以同时查看首 token 等待、输出速度、缓存命中和输出用量，并按主对话、子代理、辅助调用切换范围。</p>
              <p class="assistant-conclusion">缓存命中是输入复用率，上下文圆环是当前窗口占用；两者分别保留，读数更清楚。</p>
              <div class="reply-meta"><Icon name={running ? 'loader' : 'clock'} size={12} class={running ? 'demo-spinning' : ''} /><span>{running ? '正在生成后续内容…' : '已完成 · 18 秒'}</span></div>
            </article>
          {/if}
        </div>
        <div class="composer-area">
          {#if running}<div class="runtime-line"><span class="running-dot"></span><span>正在输出</span><span class="runtime-live">约 {liveSpeed} tok/s <span>· 当前估算</span></span></div>{/if}
          <div class="composer" data-magi-surface="composer">
            <textarea aria-label="消息输入" bind:value={draft} placeholder="继续告诉 Magi 要做什么…" rows="2" onkeydown={inputKey} oncompositionstart={() => composing = true} oncompositionend={() => { composing = false; compositionEnded = performance.now(); }}></textarea>
            <div class="composer-actions">
              <div class="composer-left">
                <button class="tool-button" title="添加附件与引用" aria-label="添加附件与引用" onclick={() => notify('附件入口保留在原位置')}><Icon name="plus" size={18} /></button>
                <button class="tool-button project-control" title="项目：magi-rust-rewrite" aria-label="选择项目" onclick={() => notify('项目选择入口保留在原位置')}><Icon name="grid" size={15} /><span class="project-name">magi-rust-rewrite</span></button>
                <button class="tool-button branch-control" title="当前分支：main" aria-label="Git 分支" onclick={() => notify('Git 分支入口保留在原位置')}><Icon name="git-branch" size={15} /><span>main</span></button>
              </div>
              <div class="composer-right">
                <button class="tool-button access-control" title="权限：只读" aria-label="权限：只读" onclick={() => notify('权限选择入口保留在原位置')}><Icon name="shield" size={14} /><span>只读</span></button>
                <button class="session-stats-trigger tool-button" class:active={open} bind:this={statsButton} onclick={toggleStats} aria-label={`会话统计：${scopeLabel}，输出速度 ${stats.tps?.toFixed(1) ?? '未提供'} tok/s，缓存命中 ${percent(stats.rate)}`} aria-expanded={open} aria-controls="session-stats-panel" aria-haspopup="dialog" title="会话统计">
                  <Icon name="stats" size={16} />
                  <span class="stats-summary"><span>{stats.tps == null ? '—' : stats.tps.toFixed(1)} <span class="summary-unit">tok/s</span></span><span class="summary-divider">·</span><span>缓存 {stats.rate == null ? '—' : percent(stats.rate)}</span></span>
                  {#if running}<span class="stats-update-dot" aria-hidden="true"></span>{/if}
                </button>
                <span class="context-control">
                  {#key contextInstance}<ContextUsageRing model="gpt-6-luna" usageRatio={scene === 'empty' ? null : .348} tokenUsed={scene === 'empty' ? null : 89088} remainingTokens={scene === 'empty' ? null : 166912} tokenLimit={256000} measurement="authoritative" contextBreakdown={{ conversationTokens: 41000, imageTokens: 0, systemInstructionTokens: 15000, projectContextTokens: 8500, skillTokens: 4000, contextReferenceTokens: 5000, builtinToolTokens: 12000, mcpToolTokens: 3588, skillToolTokens: 0, otherToolTokens: 0 }} />{/key}
                </span>
                <span class="toolbar-divider"></span>
                <button class="tool-button model-control" title="模型：GPT-6-luna · 中等推理" aria-label="选择模型" onclick={() => notify('模型与推理选择沿用 Magi 现有入口')}><span class="model-name">GPT-6-luna</span><span class="model-effort">中等</span><Icon name="chevron-down" size={11} /></button>
                <button class="tool-button enhance-control" title="增强提示词" aria-label="增强提示词" onclick={() => notify('提示词增强入口保留在原位置')}><Icon name="enhance" size={16} /></button>
                <button class="send-control" disabled={!canSend && !running} aria-label={running && !canSend ? '停止演示输出' : '发送演示消息'} onclick={submit}><Icon name={running && !canSend ? 'stop' : 'send'} size={16} /></button>
              </div>
            </div>
          </div>
          <div class="composer-caption"><span>示例数据 · 不调用模型</span><span class="desktop-hint">点击统计入口查看性能与用量</span></div>
        </div>
      </section>
      {#if layout === 'split'}
        <aside class="file-preview"><header><Icon name="file-code" size={15} /><span>InputArea.svelte</span></header><div class="file-preview-content"><span class="file-label">输入区</span><h2>把统计放回实际布局</h2><p>左侧会话缩窄时，统计入口自动收为图标，模型选择和发送按钮仍保持可用。</p><div class="code-preview"><span>&lt;InputArea&gt;</span><span>  项目 / Git / 权限</span><span>  会话统计 / 上下文</span><span>  模型 / 推理 / 发送</span><span>&lt;/InputArea&gt;</span></div><div class="preview-rule"><Icon name="check" size={14} /><span>浮层约束在会话区域内</span></div></div></aside>
      {/if}
    </div>
    <footer class="review-footer"><span><Icon name="info" size={13} />{layout === 'phone' ? '手机：点统计图标展开，保留原上下文入口' : layout === 'split' ? '窄分栏：摘要自动收起，浮层不越过会话列' : '宽屏：一眼看速度与缓存，点击查看完整统计'}</span><span>{themeStatus}</span></footer>
  </main>

  {#if open}
    <div class="session-stats-popover" id="session-stats-panel" role="dialog" aria-label="会话统计" tabindex="-1" data-magi-surface="popover" use:placeStats>
      <div class="stats-heading"><div class="stats-title"><Icon name="stats" size={17} /><h2>会话统计</h2></div><button class="popover-close" title="关闭统计" aria-label="关闭统计" onclick={() => closeStats(true)}><Icon name="close" size={15} /></button></div>
      <div class="stats-subhead"><span>当前会话{running ? ' · 生成中' : ''}</span><SelectField label="统计范围" bind:value={scope} options={scopes} /></div>
      {#if scene === 'empty'}
        <div class="stats-empty"><Icon name="chat" size={23} /><strong>会话尚未产生用量</strong><p>模型请求完成后，性能与缓存统计会出现在这里。</p></div>
      {:else}
        <div class="metric-grid">
          <div class="metric"><div class="metric-label"><Icon name="clock" size={13} /><span>首 token 平均 <span class="metric-abbr">TTFT</span></span></div><div class="metric-value">{stats.ttft == null ? '—' : stats.ttft.toFixed(1)}{#if stats.ttft != null}<span>秒</span>{/if}</div><span class="metric-caption">{stats.timed ? `${stats.timed} 次有效正文流` : '暂无有效流式样本'}</span></div>
          <div class="metric"><div class="metric-label"><Icon name="zap" size={13} /><span>平均输出速度 <span class="metric-abbr">TPS</span></span></div><div class="metric-value">{stats.tps == null ? '—' : stats.tps.toFixed(1)}{#if stats.tps != null}<span>tok/s</span>{/if}</div><span class="metric-caption">{stats.timed ? '按输出量与生成时长加权' : '未提供可配对的速度数据'}</span></div>
          <div class="metric"><div class="metric-label"><Icon name="database" size={13} /><span>缓存命中</span></div><div class="metric-value" class:unobserved={stats.rate == null}>{stats.rate == null ? '未观测' : percent(stats.rate)}</div><span class="metric-caption">{stats.observed ? '缓存读取 / 已观测完整输入' : 'Provider 未报告缓存读取'}</span></div>
          <div class="metric"><div class="metric-label"><Icon name="send" size={13} /><span>累计输出</span></div><div class="metric-value">{compact(stats.output)}<span>tok</span></div><span class="metric-caption">{stats.calls} 次模型请求</span></div>
        </div>
        {#if running}{#if scope === 'all' || scope === 'main'}<div class="live-observation"><span class="running-dot"></span><span>当前主对话约 {liveSpeed} tok/s <span>（估算）</span></span></div>{/if}<p class="pending-note">上方保留已结算统计，本次缓存用量等待返回。</p>{/if}
        <div class="stats-details">
          <button class="disclosure" aria-expanded={expanded === 'tokens'} aria-controls="token-details" onclick={() => expanded = expanded === 'tokens' ? null : 'tokens'}><Icon name="database" size={14} /><span>Token 与缓存明细</span><span class="disclosure-summary">{compact(stats.input + stats.output)} tok</span><Icon name={expanded === 'tokens' ? 'chevron-up' : 'chevron-down'} size={13} /></button>
          {#if expanded === 'tokens'}<div id="token-details" class="detail-body"><dl><div><dt>累计输入</dt><dd>{exact(stats.input)} <span>tok</span></dd></div><div><dt>缓存读取</dt><dd>{stats.observed ? exact(stats.read) : '未提供'}{#if stats.observed}<span>tok</span>{/if}</dd></div><div><dt>未缓存输入</dt><dd>{stats.observed ? exact(stats.input - stats.read) : '未提供'}{#if stats.observed}<span>tok</span>{/if}</dd></div><div><dt>缓存写入</dt><dd class="detail-muted">未提供</dd></div><div><dt>累计输出</dt><dd>{exact(stats.output)} <span>tok</span></dd></div><div class="detail-separated"><dt>请求命中率</dt><dd>{percent(stats.requestRate)}</dd></div><div><dt>命中 / 已观测请求</dt><dd>{stats.hits} / {stats.observed}</dd></div></dl><p class="detail-note">输入已包含缓存读取，不重复相加。未报告写入不按零计算；累计输出沿 Provider 口径，可能包含推理 Token。</p></div>{/if}
          <button class="disclosure" aria-expanded={expanded === 'performance'} aria-controls="performance-details" onclick={() => expanded = expanded === 'performance' ? null : 'performance'}><Icon name="clock" size={14} /><span>响应性能明细</span><span class="disclosure-summary">{stats.timed} 次有效</span><Icon name={expanded === 'performance' ? 'chevron-up' : 'chevron-down'} size={13} /></button>
          {#if expanded === 'performance'}<div id="performance-details" class="detail-body"><dl><div><dt>有效正文流 / 总请求</dt><dd>{stats.timed} / {stats.calls}</dd></div><div><dt>最近一次首 token</dt><dd>{stats.timed ? '2.8 秒' : '未提供'}</dd></div><div><dt>最近一次输出速度</dt><dd>{stats.timed ? '46.2 tok/s' : '未提供'}</dd></div></dl><p class="detail-note">TTFT 从实际发出请求到首段正文；TPS 为可配对正文 Token / 生成时长。不计工具等待，不用隐藏推理 Token 冒充正文速度。</p></div>{/if}
        </div>
        <div class="observation-footer"><Icon name="info" size={12} /><span>缓存已观测 {stats.observed} / {stats.calls} 次{scene === 'unobserved' ? ' · 未观测不等于零命中' : ''}</span></div>
      {/if}
      <div class="demo-panel-note">交互示意 · 性能与用量均为示例数据</div>
    </div>
  {/if}
  {#if feedback}<div class="demo-toast" role="status">{feedback}</div>{/if}
</div>
