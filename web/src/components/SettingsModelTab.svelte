<script lang="ts">
  import {
    resolveModelConfigTabStatus,
  } from '../shared/model-governance';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';
  import type { AgentBinding } from '../shared/types/registry-types';
  import type { RoleTemplate } from '../shared/types/role-templates';
  import type { VisionBuiltinTextModelRule } from '../shared/settings-bootstrap';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ModelConfigForm from './ModelConfigForm.svelte';

  let {
    modelConfigTab = $bindable(),
    orchConfig = $bindable(),
    compConfig = $bindable(),
    visionConfig = $bindable(),
    visionBuiltinTextModelRules,
    imageConfig = $bindable(),
    workerConfigs = $bindable(),
    modelConfigBaselines,
    workerModelTabs,
    modelStatuses,
    saveStatus,
    testStatus,
    fetchingModels,
    keyVisible = $bindable(),
    modelDropdownOpen,
    dropdownPosition,
    modelLists,
    roleTemplates,
    registryAgents,
    getBaseUrlPlaceholder,
    shouldRecommendStandardUrlMode,
    openModelDropdown,
    closeModelDropdown,
    fetchModelList,
    selectModel,
    saveModelConfig,
    testModelConnection,
    getStatusClass,
    getStatusText,
    getWorkerDisplayName,
    getAgentColor,
    deleteEngine,
    openAddEngineDialog,
    renameEngineDisplay,
    openWebModelSettings
  } = $props<{
    modelConfigTab: string;
    orchConfig: any;
    compConfig: any;
    visionConfig: any;
    visionBuiltinTextModelRules: VisionBuiltinTextModelRule[];
    imageConfig: any;
    workerConfigs: Record<string, any>;
    modelConfigBaselines: Record<string, any>;
    workerModelTabs: string[];
    modelStatuses: Record<string, { status?: string }>;
    saveStatus: Record<string, string>;
    testStatus: Record<string, string>;
    fetchingModels: Record<string, boolean>;
    keyVisible: Record<string, boolean>;
    modelDropdownOpen: Record<string, boolean>;
    dropdownPosition: any;
    modelLists: Record<string, string[]>;
    roleTemplates: RoleTemplate[];
    registryAgents: AgentBinding[];
    getBaseUrlPlaceholder: () => string;
    shouldRecommendStandardUrlMode: (baseUrl: string) => boolean;
    openModelDropdown: (type: string, target: HTMLElement) => void;
    closeModelDropdown: (key: string) => void;
    fetchModelList: (type: 'orch' | 'comp' | 'vision' | 'image' | 'worker', statusKey: string) => void;
    selectModel: (type: string, model: string) => void;
    saveModelConfig: (type: 'orch' | 'comp' | 'vision' | 'image' | 'worker', statusKey: string) => void;
    testModelConnection: (type: 'orch' | 'comp' | 'vision' | 'image' | 'worker', statusKey: string) => void;
    getStatusClass: (status: string) => string;
    getStatusText: (status: string) => string;
    getWorkerDisplayName: (workerId: string) => string;
    getAgentColor: (templateId: string, colorToken?: string) => any;
    deleteEngine: (engineId: string) => void;
    /** 打开 GPT Web 的设置（登录、可用性、工具通道）。它依赖内置浏览器的会话，所以设置在「浏览器」页。 */
    openWebModelSettings: () => void;
    openAddEngineDialog: () => void;
    renameEngineDisplay: (engineId: string, newName: string) => void;
  }>();

  // 角色 displayName 解析（带 i18n fallback）
  function resolveTemplateDisplayName(templateId: string): string {
    const tmpl = roleTemplates.find((t: RoleTemplate) => t.templateId === templateId);
    if (!tmpl) return templateId;
    const key = tmpl.i18n?.displayNameKey || `roleTemplate.${tmpl.templateId}.displayName`;
    const translated = i18n.t(key);
    return translated !== key ? translated : tmpl.displayName;
  }

  // 反向 lookup：每个引擎服务于哪些角色
  // engineId 空串 = 继承编排模型；非空 = 显式绑定到该 engine。
  const inheritedConsumers = $derived(
    registryAgents.filter((a: AgentBinding) => !a.engineId)
  );

  // 概览只列真正有代理角色在用的引擎；没绑定角色的引擎在上方标签里已经能看到，这里折成一行，
  // 避免引擎一多，概览就为一堆"闲置"行把页面拉长。
  const boundEngineIds = $derived(
    workerModelTabs.filter((id: string) => registryAgents.some((a: AgentBinding) => a.engineId === id)),
  );
  const idleEngineCount = $derived(workerModelTabs.length - boundEngineIds.length);

  function consumersOf(engineId: string): AgentBinding[] {
    return registryAgents.filter(
      (a: AgentBinding) => a.engineId === engineId,
    );
  }

  // --- tab 条横向滚动状态：两端出现渐隐提示，切换时把当前项滚入可视区 ---
  let tabbarWrapperEl: HTMLElement | undefined = $state();
  let canScrollLeft = $state(false);
  let canScrollRight = $state(false);

  // 浮层式滚动条：原生滚动条会一直占位，这里用悬浮在标签条底边的滑块，
  // 鼠标移进标签条才出现、移出就隐藏，不占布局空间，且可拖动。
  let thumbLeft = $state(0);
  let thumbWidth = $state(0);
  let tabbarHovered = $state(false);
  let thumbDragging = $state(false);

  function updateScrollState() {
    const el = tabbarWrapperEl?.querySelector('.tabbar-scroll') as HTMLElement | null;
    if (!el) return;
    canScrollLeft = el.scrollLeft > 2;
    canScrollRight = el.scrollLeft + el.clientWidth < el.scrollWidth - 2;
    const overflow = el.scrollWidth > el.clientWidth + 1;
    thumbWidth = overflow ? Math.max(28, (el.clientWidth / el.scrollWidth) * el.clientWidth) : 0;
    const maxScroll = el.scrollWidth - el.clientWidth;
    thumbLeft = overflow && maxScroll > 0
      ? (el.scrollLeft / maxScroll) * (el.clientWidth - thumbWidth)
      : 0;
  }

  // 在标签上按住鼠标左右拖动来滚动标签条。浏览器只对触摸屏提供“拖动即滚动”，
  // 鼠标按下后得到的是 click / 文本选择，所以要自己处理：位移超过阈值才算拖动，
  // 否则仍是普通点击；拖动结束后吞掉紧跟着的那次 click，避免松手时误切换标签。
  const PAN_THRESHOLD_PX = 4;
  let tabPanning = $state(false);

  function startTabPan(event: PointerEvent) {
    if (event.button !== 0 || event.pointerType !== 'mouse') return;
    const scroller = event.currentTarget as HTMLElement;
    if (scroller.scrollWidth <= scroller.clientWidth) return;
    if ((event.target as HTMLElement).closest('input')) return;
    const startX = event.clientX;
    const startScroll = scroller.scrollLeft;
    let moved = false;
    const move = (e: PointerEvent) => {
      const dx = e.clientX - startX;
      if (!moved) {
        if (Math.abs(dx) < PAN_THRESHOLD_PX) return;
        moved = true;
        tabPanning = true;
        scroller.style.scrollBehavior = 'auto';
        try { scroller.setPointerCapture(e.pointerId); } catch { /* 合成事件没有有效的 pointerId */ }
      }
      scroller.scrollLeft = startScroll - dx;
    };
    const end = () => {
      scroller.removeEventListener('pointermove', move);
      scroller.removeEventListener('pointerup', end);
      scroller.removeEventListener('pointercancel', end);
      scroller.style.scrollBehavior = '';
      tabPanning = false;
      if (moved) {
        const swallow = (e: Event) => { e.stopPropagation(); e.preventDefault(); };
        scroller.addEventListener('click', swallow, { capture: true, once: true });
        setTimeout(() => scroller.removeEventListener('click', swallow, true), 0);
      }
    };
    scroller.addEventListener('pointermove', move);
    scroller.addEventListener('pointerup', end);
    scroller.addEventListener('pointercancel', end);
  }

  function startThumbDrag(event: PointerEvent) {
    const scroller = tabbarWrapperEl?.querySelector('.tabbar-scroll') as HTMLElement | null;
    if (!scroller) return;
    event.preventDefault();
    const thumb = event.currentTarget as HTMLElement;
    thumb.setPointerCapture(event.pointerId);
    const startX = event.clientX;
    const startScroll = scroller.scrollLeft;
    const scrollable = scroller.scrollWidth - scroller.clientWidth;
    const travel = Math.max(1, scroller.clientWidth - thumbWidth);
    scroller.style.scrollBehavior = 'auto';
    thumbDragging = true;
    const move = (e: PointerEvent) => {
      scroller.scrollLeft = startScroll + ((e.clientX - startX) / travel) * scrollable;
    };
    const end = () => {
      thumbDragging = false;
      scroller.style.scrollBehavior = '';
      thumb.removeEventListener('pointermove', move);
      thumb.removeEventListener('pointerup', end);
      thumb.removeEventListener('pointercancel', end);
    };
    thumb.addEventListener('pointermove', move);
    thumb.addEventListener('pointerup', end);
    thumb.addEventListener('pointercancel', end);
  }

  function scrollTabIntoView(tabId: string) {
    requestAnimationFrame(() => {
      const btn = tabbarWrapperEl?.querySelector(`.role-tab[data-tab-id="${CSS.escape(tabId)}"]`) as HTMLElement | null;
      btn?.scrollIntoView({ behavior: 'smooth', inline: 'nearest', block: 'nearest' });
      setTimeout(updateScrollState, 200);
    });
  }

  /** 鼠标滚轮的纵向滚动在 tab 条上转成横向，引擎多时不必拖动或按住 Shift。 */
  function wheelToHorizontal(event: WheelEvent) {
    const el = event.currentTarget as HTMLElement;
    if (el.scrollWidth <= el.clientWidth || Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return;
    el.scrollLeft += event.deltaY;
    event.preventDefault();
  }

  $effect(() => {
    workerModelTabs;
    modelConfigTab;
    requestAnimationFrame(updateScrollState);
  });

  // 容器宽度或标签内容变化（窗口缩放、数据晚到、重命名）都要重新计算滑块。
  $effect(() => {
    const scroller = tabbarWrapperEl?.querySelector('.tabbar-scroll') as HTMLElement | null;
    const track = tabbarWrapperEl?.querySelector('.tabbar-track') as HTMLElement | null;
    if (!scroller || !track) return;
    const observer = new ResizeObserver(() => updateScrollState());
    observer.observe(scroller);
    observer.observe(track);
    return () => observer.disconnect();
  });

  type RailItem = { id: string; name: string; sub: string; statusClass: string; statusText: string };

  function railStatusKey(tabId: string): string {
    return tabId === 'image' ? 'imageGeneration' : tabId;
  }

  function tabStatus(tabId: string): { statusClass: string; statusText: string } {
    const status = resolveModelConfigTabStatus(railStatusKey(tabId), modelStatuses);
    return { statusClass: getStatusClass(status), statusText: getStatusText(status) };
  }

  function railItem(id: string, name: string, model: string | undefined): RailItem {
    const status = tabStatus(id);
    return { id, name, sub: model?.trim() || status.statusText, ...status };
  }

  const roleRailItems = $derived<RailItem[]>([
    { ...railItem('orch', i18n.t('settings.model.orchestratorModel'), undefined), sub: i18n.t('settings.model.sessionModelSelection') },
    railItem('comp', i18n.t('settings.model.auxiliaryModel'), compConfig?.model),
    railItem('image', i18n.t('settings.model.imageGenerationModel'), imageConfig?.model),
    railItem('vision', i18n.t('settings.model.visionModel'), visionConfig?.model),
  ]);

  const workerRailItems = $derived<RailItem[]>(
    workerModelTabs.map((id: string) => railItem(id, getWorkerDisplayName(id), workerConfigs[id]?.model)),
  );

  function tabTitle(tabId: string): string {
    return roleRailItems.find((item) => item.id === tabId)?.name ?? getWorkerDisplayName(tabId);
  }

  // --- Inline rename 状态机 ---
  let editingTab = $state<string | null>(null);
  let editingName = $state('');
  let renameInputEl: HTMLInputElement | undefined = $state();

  function startRename(engineId: string) {
    if (engineId === 'orch' || engineId === 'comp' || engineId === 'image') return;
    editingTab = engineId;
    editingName = getWorkerDisplayName(engineId);
    requestAnimationFrame(() => {
      renameInputEl?.focus();
      renameInputEl?.select();
    });
  }

  function commitRename() {
    if (!editingTab) return;
    const trimmed = editingName.trim();
    if (trimmed) {
      renameEngineDisplay(editingTab, trimmed);
    }
    editingTab = null;
    editingName = '';
  }

  function cancelRename() {
    editingTab = null;
    editingName = '';
  }

  function onRenameKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelRename();
    }
  }

  function selectTab(tabId: string) {
    modelConfigTab = tabId;
    scrollTabIntoView(tabId);
  }
</script>

<div class="apple-manager settings-tab-inner">
  <div class="apple-scroller-proxy">
    <div class="settings-section">
      <div
        class="tabbar-wrapper"
        bind:this={tabbarWrapperEl}
        class:can-scroll-left={canScrollLeft}
        class:can-scroll-right={canScrollRight}
        role="presentation"
        onmouseenter={() => { tabbarHovered = true; }}
        onmouseleave={() => { tabbarHovered = false; }}
      >
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="tabbar-scroll"
          class:panning={tabPanning}
          onscroll={updateScrollState}
          onwheel={wheelToHorizontal}
          onpointerdown={startTabPan}
        >
          <div class="tabbar-track" role="tablist" aria-label={i18n.t('settings.zone.quickStart')}>
            <span class="tab-group-label" aria-hidden="true">{i18n.t('settings.model.tabGroup.roles')}</span>
            {#each roleRailItems as item (item.id)}
              <button
                type="button"
                class="role-tab"
                class:active={modelConfigTab === item.id}
                role="tab"
                aria-selected={modelConfigTab === item.id}
                data-tab-id={item.id}
                title={item.sub}
                onclick={() => selectTab(item.id)}
              >
                <span class="role-tab-status {item.statusClass}" title={item.statusText}></span>
                <span class="role-tab-name">{item.name}</span>
              </button>
            {/each}

            <span class="tab-group-divider" aria-hidden="true"></span>
            <span class="tab-group-label" aria-hidden="true">{i18n.t('settings.model.tabGroup.engines')}</span>
            {#each workerRailItems as item (item.id)}
              {@const isActive = modelConfigTab === item.id}
              {@const isEditing = editingTab === item.id}
              <button
                type="button"
                class="role-tab role-tab--worker"
                class:active={isActive}
                class:editing={isEditing}
                role="tab"
                aria-selected={isActive}
                data-tab-id={item.id}
                onclick={() => { if (!isEditing) selectTab(item.id); }}
                ondblclick={(e) => { e.stopPropagation(); startRename(item.id); }}
                onkeydown={(e) => {
                  if (isEditing) return;
                  if (e.key === 'F2') { e.preventDefault(); startRename(item.id); }
                  else if (e.key === 'Delete') { e.preventDefault(); deleteEngine(item.id); }
                }}
                title={isEditing ? '' : `${item.sub}\n${i18n.t('settings.model.renameEngineHint')}`}
              >
                <span class="role-tab-status {item.statusClass}" title={item.statusText}></span>
                {#if isEditing}
                  <input
                    bind:this={renameInputEl}
                    class="role-tab-rename-input"
                    type="text"
                    bind:value={editingName}
                    onkeydown={onRenameKeydown}
                    onblur={commitRename}
                    onclick={(e) => e.stopPropagation()}
                    onmousedown={(e) => e.stopPropagation()}
                  />
                {:else}
                  <span class="role-tab-name">{item.name}</span>
                  <!-- svelte-ignore a11y_click_events_have_key_events -->
                  <span
                    class="role-tab-delete"
                    role="button"
                    tabindex="-1"
                    title={i18n.t('settings.model.deleteEngine')}
                    onclick={(e) => {
                      e.stopPropagation();
                      deleteEngine(item.id);
                    }}
                  >×</span>
                {/if}
              </button>
            {/each}

            <button type="button" class="role-tab role-tab--add" onclick={openAddEngineDialog}>
              <Icon name="plus" size={12} />
              <span>{i18n.t('settings.model.addEngine')}</span>
            </button>
          </div>
        </div>
        {#if thumbWidth > 0}
          <div class="tabbar-thumb-track" class:visible={tabbarHovered || thumbDragging} aria-hidden="true">
            <!-- 仅供鼠标拖动的装饰性滑块；键盘用户通过 tab 焦点滚动，无需也不应暴露为控件。 -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="tabbar-thumb"
              class:dragging={thumbDragging}
              style="left:{thumbLeft}px;width:{thumbWidth}px"
              onpointerdown={startThumbDrag}
            ></div>
          </div>
        {/if}
      </div>

      <div class="tab-content-area">
        {#if modelConfigTab === 'orch'}
          <ModelConfigForm
            formType="orch"
            statusKey="orch"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={orchConfig}
            baselineConfig={modelConfigBaselines.orch}
            bind:keyVisible
            showModelField={false}
            showAdvancedOptions={false}
            description={i18n.t('settings.model.orchestratorDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if modelConfigTab === 'comp'}
          <ModelConfigForm
            formType="comp"
            statusKey="comp"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={compConfig}
            baselineConfig={modelConfigBaselines.comp}
            bind:keyVisible
            showAdvancedOptions={false}
            description={i18n.t('settings.model.auxiliaryDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if modelConfigTab === 'image'}
          <ModelConfigForm
            formType="image"
            statusKey="image"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={imageConfig}
            baselineConfig={modelConfigBaselines.image}
            bind:keyVisible
            showAdvancedOptions={false}
            description={i18n.t('settings.model.imageGenerationDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if modelConfigTab === 'vision'}
          <ModelConfigForm
            formType="vision"
            statusKey="vision"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={visionConfig}
            baselineConfig={modelConfigBaselines.vision}
            {visionBuiltinTextModelRules}
            bind:keyVisible
            showAdvancedOptions={false}
            description={i18n.t('settings.model.visionDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if workerConfigs[modelConfigTab]}
          <ModelConfigForm
            formType="worker"
            statusKey={modelConfigTab}
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={workerConfigs[modelConfigTab]}
            baselineConfig={modelConfigBaselines[modelConfigTab]}
            bind:keyVisible
            showAdvancedOptions={true}
            description={null}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else}
          <div class="llm-config-empty">
            <div class="llm-config-empty-inner">
              <Icon name="plus" size={24} />
              <p>{i18n.t('settings.model.noWorkerConfig')}</p>
            </div>
          </div>
        {/if}
      </div>
    </div>

    <div class="settings-section engine-usage-section">
      <div class="settings-section-header">
        <div class="settings-section-title">{i18n.t('settings.model.engineUsageTitle')}</div>
        <div class="settings-section-subtitle">{i18n.t('settings.model.engineUsageSubtitle')}</div>
      </div>

      <div class="engine-usage-list">
        <div class="engine-usage-row engine-usage-row--system">
          <div class="engine-avatar engine-avatar--primary" aria-hidden="true">
            <Icon name="chat" size={15} />
            <span
              class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('orch', modelStatuses))}"
              title={getStatusText(resolveModelConfigTabStatus('orch', modelStatuses))}
            ></span>
          </div>
          <div class="engine-identity">
            <span class="engine-name">{i18n.t('settings.model.orchestratorModel')}</span>
            <span class="engine-model-tag">{i18n.t('settings.model.sessionModelSelection')}</span>
          </div>
          <div class="engine-consumers engine-consumers--stacked">
            <span class="engine-system-note">{i18n.t('settings.model.orchestratorSystemUsage')}</span>
            {#if inheritedConsumers.length > 0}
              <div class="consumer-chip-list">
                {#each inheritedConsumers as agent (agent.templateId)}
                  {@const color = getAgentColor(agent.templateId)}
                  <span
                    class="consumer-chip"
                    style="background: {color.muted}; color: {color.color}"
                  >{resolveTemplateDisplayName(agent.templateId)}</span>
                {/each}
              </div>
            {/if}
          </div>
        </div>

        {#if boundEngineIds.length > 0}
          {#each boundEngineIds as workerId (workerId)}
            {@const consumers = consumersOf(workerId)}
            {@const workerStatus = resolveModelConfigTabStatus(workerId, modelStatuses)}
            {@const indicatorVariant = resolveAgentIndicatorVariant(workerStatus)}
            {@const workerColor = getAgentColor(workerId)}
            <div class="engine-usage-row">
              <div
                class="engine-avatar"
                style="background: {workerColor.muted}; color: {workerColor.color}"
                aria-hidden="true"
              >
                <Icon name="bot" size={15} />
                <span
                  class="worker-dot"
                  class:brand={indicatorVariant === 'brand'}
                  class:disabled={indicatorVariant === 'disabled'}
                  class:warning={indicatorVariant === 'warning'}
                  class:error={indicatorVariant === 'error'}
                  style="--worker-brand-color: {workerColor.color}"
                  title={getStatusText(workerStatus)}
                ></span>
              </div>
              <div class="engine-identity">
                <span class="engine-name">{getWorkerDisplayName(workerId)}</span>
                {#if workerConfigs[workerId]?.model}
                  <span class="engine-model-tag">{workerConfigs[workerId].model}</span>
                {/if}
              </div>
              <div class="engine-consumers">
                {#if consumers.length > 0}
                  {#each consumers as agent (agent.templateId)}
                    {@const color = getAgentColor(agent.templateId)}
                    <span
                      class="consumer-chip"
                      style="background: {color.muted}; color: {color.color}"
                    >{resolveTemplateDisplayName(agent.templateId)}</span>
                  {/each}
                {:else}
                  <span class="engine-empty">{i18n.t('settings.model.engineIdle')}</span>
                {/if}
              </div>
            </div>
          {/each}
        {/if}
        {#if idleEngineCount > 0}
          <div class="engine-idle-note">{i18n.t('settings.model.enginesIdleSummary', { count: idleEngineCount })}</div>
        {/if}
      </div>
    </div>

    <div class="settings-section web-model-pointer">
      <div class="settings-section-header">
        <div class="settings-section-title">{i18n.t('settings.model.webModelTitle')}</div>
        <div class="settings-section-subtitle">{i18n.t('settings.model.webModelSubtitle')}</div>
      </div>
      <button type="button" class="web-model-pointer-button" onclick={openWebModelSettings}>
        <Icon name="globe" size={14} />
        <span>{i18n.t('settings.model.webModelOpen')}</span>
      </button>
    </div>
  </div>
</div>

<style>
  .web-model-pointer {
    margin-top: var(--space-4);
  }

  .web-model-pointer-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-xs);
  }

  .web-model-pointer-button:hover {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
  }

  .apple-manager {
    container: settings-model / inline-size;
  }

  /* ===== 统一 tab 条（沿用「角色」tab underline 风格） ===== */
  .tabbar-wrapper {
    position: relative;
    margin-bottom: var(--space-4);
    --fade-w: 24px;
  }
  .tabbar-wrapper::before,
  .tabbar-wrapper::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    width: var(--fade-w);
    pointer-events: none;
    z-index: 1;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }
  .tabbar-wrapper::before {
    left: 0;
    background: linear-gradient(to right, var(--background), transparent);
  }
  .tabbar-wrapper::after {
    right: 0;
    background: linear-gradient(to left, var(--background), transparent);
  }
  .tabbar-wrapper.can-scroll-left::before { opacity: 1; }
  .tabbar-wrapper.can-scroll-right::after { opacity: 1; }

  .tabbar-scroll {
    overflow-x: auto;
    overflow-y: hidden;
    scroll-behavior: smooth;
    -webkit-overflow-scrolling: touch;
    scrollbar-width: none;
  }
  .tabbar-scroll::-webkit-scrollbar { height: 0; }
  .tabbar-scroll.panning { cursor: grabbing; user-select: none; }
  .tabbar-scroll.panning :global(.role-tab) { cursor: grabbing; }

  /* 悬浮滑块：贴在标签条底边，不占布局高度；移进标签条出现、移出隐藏。 */
  .tabbar-thumb-track {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 5px;
    pointer-events: none;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }
  .tabbar-thumb-track.visible { opacity: 1; }
  .tabbar-thumb {
    position: absolute;
    top: 0;
    bottom: 0;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--foreground) 26%, transparent);
    pointer-events: auto;
    cursor: grab;
    touch-action: none;
  }
  .tabbar-thumb:hover, .tabbar-thumb.dragging {
    background: color-mix(in srgb, var(--foreground) 46%, transparent);
  }
  .tabbar-thumb.dragging { cursor: grabbing; }

  .tabbar-track {
    display: flex;
    align-items: stretch;
    gap: 2px;
    min-width: max-content;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .role-tab {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 11px 9px;
    border: none;
    background: transparent;
    color: var(--ind-foreground-muted);
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    letter-spacing: -0.005em;
    cursor: pointer;
    transition: color 0.15s ease;
    white-space: nowrap;
  }
  .role-tab:hover {
    color: var(--ind-foreground-secondary);
  }
  .role-tab.active {
    color: var(--ind-foreground);
    font-weight: 600;
  }
  .role-tab.active::after {
    content: '';
    position: absolute;
    left: 11px;
    right: 11px;
    bottom: -1px;
    height: 2px;
    background: var(--ind-tab-accent);
    border-radius: 2px;
  }
  .role-tab:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--ind-tab-accent) 60%, transparent);
    outline-offset: -3px;
    border-radius: 4px;
  }

  .role-tab-name {
    font-variant-numeric: tabular-nums;
  }

  /* 状态指示点：5px 圆点，沿用角色 tab 视觉 */
  .role-tab-status {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    flex-shrink: 0;
    margin-left: 1px;
    background: var(--ind-foreground-soft, var(--foreground-muted));
  }
  .role-tab-status.success { background: var(--success, #34c759); }
  .role-tab-status.checking { background: var(--warning, #d97706); }
  .role-tab-status.warning { background: var(--warning, #d97706); }
  .role-tab-status.error { background: var(--error, #ff3b30); }
  .role-tab-status.disabled { background: color-mix(in srgb, var(--ind-foreground-soft) 55%, transparent); }

  /* 代理引擎 tab 的状态点支持品牌色变体 */
  .role-tab-status.worker-dot.brand { background: var(--worker-brand-color); }

  /* 删除按钮：hover 浮出 */
  .role-tab-delete {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    font-size: 13px;
    line-height: 1;
    color: var(--ind-foreground-muted);
    opacity: 0;
    transition: opacity 0.15s ease, background 0.15s ease, color 0.15s ease;
    cursor: pointer;
    margin-left: 2px;
  }
  .role-tab--worker:hover .role-tab-delete { opacity: 0.7; }
  .role-tab-delete:hover {
    opacity: 1 !important;
    background: color-mix(in srgb, var(--error, #ff3b30) 12%, transparent);
    color: var(--error, #ff3b30);
  }

  /* + 新增引擎按钮 */
  .role-tab--add {
    color: var(--ind-foreground-soft, var(--ind-foreground-muted));
    padding: 7px 9px 9px;
    white-space: nowrap;
  }

  /* 固定角色与自建引擎是两类东西：分组标签 + 分隔线，而不是一排同质的标签。 */
  .tab-group-label {
    align-self: center;
    padding: 0 6px 2px 4px;
    color: var(--ind-foreground-soft, var(--ind-foreground-muted));
    font-size: 11px;
    white-space: nowrap;
  }
  .tab-group-divider {
    align-self: center;
    width: 1px;
    height: 16px;
    margin: 0 6px 2px;
    background: var(--ind-border-separator);
  }
  .role-tab--add:hover {
    color: var(--ind-tab-accent);
  }

  /* Inline rename input */
  .role-tab-rename-input {
    background: transparent;
    border: none;
    outline: none;
    font-family: inherit;
    font-size: 13px;
    font-weight: 600;
    color: var(--ind-foreground);
    padding: 0;
    margin: 0;
    width: 9ch;
    min-width: 4ch;
    border-bottom: 1px dashed var(--ind-tab-accent);
    border-radius: 0;
    letter-spacing: -0.005em;
  }
  .role-tab.editing { cursor: text; }

  .tab-content-area {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  /* 概览列表内的连接状态 */
  .model-status-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    background: var(--foreground-muted);
    flex-shrink: 0;
  }
  .model-status-dot.success { background: var(--success, #16a34a); }
  .model-status-dot.checking { background: var(--warning, #d97706); }
  .model-status-dot.warning { background: var(--warning, #d97706); }
  .model-status-dot.error { background: var(--error, #dc2626); }
  .model-status-dot.disabled { background: var(--foreground-muted, #94a3b8); }

  .worker-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
    background: var(--foreground-muted, #94a3b8);
  }
  .worker-dot.brand { background: var(--worker-brand-color); }
  .worker-dot.disabled { background: var(--foreground-muted, #94a3b8); }
  .worker-dot.warning { background: var(--warning, #d97706); }
  .worker-dot.error { background: var(--error, #dc2626); }

  .llm-config-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: 1;
    color: var(--foreground-muted);
    padding: var(--space-6) 0;
  }
  .llm-config-empty-inner {
    text-align: center;
  }
  .llm-config-empty-inner p {
    margin-top: 8px;
  }

  /* ===== 引擎用途概览 ===== */
  .engine-usage-section {
    margin-top: var(--space-4);
  }
  .engine-usage-section .settings-section-header {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: flex-start;
    gap: 4px;
    margin-bottom: var(--space-3);
  }
  .settings-section-subtitle {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: 1.5;
  }
  .engine-usage-list {
    display: flex;
    flex-direction: column;
    background: var(--ind-bg-control, var(--surface-2));
    border: 1px solid var(--ind-border-control, var(--border));
    border-radius: 8px;
    overflow: hidden;
  }
  .engine-usage-row {
    display: grid;
    grid-template-columns: 34px minmax(130px, 176px) minmax(0, 1fr);
    column-gap: 12px;
    align-items: center;
    padding: 12px 14px;
  }
  .engine-usage-row + .engine-usage-row {
    border-top: 1px solid var(--ind-border-separator, var(--border-subtle, var(--border)));
  }
  .engine-avatar {
    position: relative;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--ind-foreground-secondary, var(--foreground));
    background: var(--ind-bg-control-hover, var(--surface-3));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 10%, transparent);
  }
  .engine-avatar--primary {
    color: var(--ind-tab-accent, var(--info));
    background: color-mix(in srgb, var(--ind-tab-accent, var(--info)) 11%, var(--ind-bg-control, var(--surface-2)));
  }
  .engine-avatar > .model-status-dot,
  .engine-avatar > .worker-dot {
    position: absolute;
    right: -2px;
    bottom: -2px;
    border: 2px solid var(--ind-bg-control, var(--surface-2));
    box-sizing: content-box;
  }
  .engine-identity {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .engine-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--ind-foreground, var(--foreground));
    line-height: 1.35;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .engine-model-tag {
    font-size: var(--text-xs);
    color: var(--ind-foreground-soft, var(--foreground-muted));
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, monospace);
    line-height: 1.35;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    align-self: flex-start;
  }
  .engine-consumers {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    align-content: center;
    min-width: 0;
  }
  .engine-consumers--stacked {
    flex-direction: column;
    flex-wrap: nowrap;
    align-items: flex-start;
    gap: 8px;
  }
  .consumer-chip-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .consumer-chip {
    display: inline-flex;
    align-items: center;
    min-height: 22px;
    padding: 3px 9px;
    border-radius: var(--radius-full);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
    line-height: 1;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 10%, transparent);
  }
  .engine-idle-note {
    padding: var(--space-2) var(--space-3);
    color: var(--ind-foreground-muted, var(--foreground-muted));
    font-size: var(--text-xs);
  }
  .engine-empty,
  .engine-system-note {
    font-size: var(--text-xs);
    color: var(--ind-foreground-muted, var(--foreground-muted));
    line-height: 1.5;
  }

  @container settings-model (max-width: 640px) {
    .role-tab--worker .role-tab-delete { opacity: 1; }
    .engine-usage-row {
      grid-template-columns: 34px minmax(0, 1fr);
      column-gap: 12px;
      row-gap: 8px;
      align-items: center;
    }
    .engine-avatar { grid-row: 1 / span 2; align-self: start; }
    .engine-identity { grid-column: 2; }
    .engine-consumers {
      grid-column: 2;
    }
  }

  @media (max-width: 768px) {
    .role-tab--worker .role-tab-delete { opacity: 1; }
  }
</style>
