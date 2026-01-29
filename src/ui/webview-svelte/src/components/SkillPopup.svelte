<script lang="ts">
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import { ensureArray, generateId } from '../lib/utils';
  import { addThreadMessage } from '../stores/messages.svelte';

  interface InstructionSkill {
    name: string;
    description?: string;
    repositoryName?: string;
    allowedTools?: string[];
    userInvocable?: boolean;
    disableModelInvocation?: boolean;
    argumentHint?: string;
    content?: string;
  }

  interface Props {
    visible: boolean;
    onClose: () => void;
  }

  let { visible, onClose }: Props = $props();

  let skillsConfig = $state<any>(null);
  let searchQuery = $state('');
  let selectedSkill = $state<InstructionSkill | null>(null);
  let argsInput = $state('');
  let selectedAgent = $state('');

  // 使用 $derived.by 来处理需要复杂逻辑的派生值
  const instructionSkills = $derived.by(() => {
    return ensureArray<InstructionSkill>(skillsConfig?.instructionSkills);
  });

  const filteredSkills = $derived.by(() => {
    const skills = instructionSkills;
    if (!searchQuery.trim()) return skills;
    const query = searchQuery.toLowerCase();
    return skills.filter(skill => {
      const name = (skill.name || '').toLowerCase();
      const desc = (skill.description || '').toLowerCase();
      return name.includes(query) || desc.includes(query);
    });
  });

  const previewContent = $derived((selectedSkill?.content || '').trim());
  const isInvocable = $derived(selectedSkill?.userInvocable !== false);

  function selectSkill(skill: InstructionSkill) {
    selectedSkill = skill;
    argsInput = '';
  }

  function closePopup() {
    selectedSkill = null;
    argsInput = '';
    searchQuery = '';
    onClose();
  }

  function applySkill() {
    if (!selectedSkill || !isInvocable) return;

    const content = argsInput.trim()
      ? `使用 Skill: ${selectedSkill.name}\n${argsInput.trim()}`
      : `使用 Skill: ${selectedSkill.name}`;

    addThreadMessage({
      id: generateId(),
      role: 'user',
      source: 'orchestrator',
      content,
      timestamp: Date.now(),
      isStreaming: false,
      isComplete: true,
    });

    vscode.postMessage({
      type: 'applyInstructionSkill',
      skillName: selectedSkill.name,
      args: argsInput.trim(),
      agent: selectedAgent || null,
      requestId: generateId(),
    });

    closePopup();
  }

  $effect(() => {
    if (visible) {
      vscode.postMessage({ type: 'loadSkillsConfig' });
    }
  });

  $effect(() => {
    const handler = (event: MessageEvent) => {
      const msg = event.data;
      if (msg.type === 'skillsConfigLoaded') {
        skillsConfig = msg.config || null;
      }
    };
    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  });
</script>

{#if visible}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={closePopup} onkeydown={(e) => e.key === 'Escape' && closePopup()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog skill-use-dialog" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>使用 Skill</h3>
        <button class="modal-close" onclick={closePopup} title="关闭">
          <Icon name="close" size={14} />
        </button>
      </div>
      <div class="modal-body">
        <div class="skill-use-layout">
          <div class="skill-use-list">
            <div class="skill-use-search">
              <input type="text" bind:value={searchQuery} placeholder="搜索 Skill...">
            </div>
            <div class="skill-use-items">
              {#if filteredSkills.length === 0}
                <div class="skill-use-empty">暂无可用技能，请先在设置中安装 Skill</div>
              {:else}
                {#each filteredSkills as skill (skill.name)}
                  <button
                    type="button"
                    class="skill-use-item"
                    class:active={selectedSkill?.name === skill.name}
                    onclick={() => selectSkill(skill)}
                  >
                    <div class="skill-use-name">{skill.name}</div>
                    <div class="skill-use-desc-row">
                      <div class="skill-use-desc">{skill.description || '-'}</div>
                    </div>
                  </button>
                {/each}
              {/if}
            </div>
          </div>

          <div class="skill-use-detail">
            {#if selectedSkill}
              <div class="skill-use-detail-scroll">
                <div class="skill-use-detail-header">
                  <div class="skill-use-title">{selectedSkill.name}</div>
                  <div class="skill-use-meta">{selectedSkill.description || ''}</div>
                  <div class="skill-use-chips">
                    {#if selectedSkill.repositoryName}
                      <span class="skill-use-chip">来源: {selectedSkill.repositoryName}</span>
                    {/if}
                    {#if ensureArray(selectedSkill.allowedTools).length > 0}
                      <span class="skill-use-chip">工具: {ensureArray(selectedSkill.allowedTools).join(', ')}</span>
                    {/if}
                    {#if selectedSkill.disableModelInvocation}
                      <span class="skill-use-chip">需手动触发</span>
                    {/if}
                  </div>
                </div>

                {#if !isInvocable}
                  <div class="skill-use-warning">该 Skill 禁止手动调用</div>
                {/if}

                <div class="skill-use-field">
                  <label for="skill-args-input">参数（可选）</label>
                  <textarea
                    id="skill-args-input"
                    bind:value={argsInput}
                    placeholder={selectedSkill.argumentHint || '输入参数'}
                  ></textarea>
                </div>

                <div class="skill-use-field">
                  <div class="skill-use-label" id="skill-preview-label">指令预览</div>
                  <div class="skill-use-preview" aria-labelledby="skill-preview-label">{previewContent || '无指令内容'}</div>
                </div>

                <div class="skill-use-field">
                  <label for="skill-agent-select">执行模型</label>
                  <select id="skill-agent-select" class="skill-use-select" bind:value={selectedAgent}>
                    <option value="">自动</option>
                    <option value="claude">Claude</option>
                    <option value="codex">Codex</option>
                    <option value="gemini">Gemini</option>
                  </select>
                </div>
              </div>
            {:else}
              <div class="skill-use-empty">请从左侧选择一个技能</div>
            {/if}
          </div>
        </div>
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" onclick={closePopup}>取消</button>
        <button class="modal-btn primary" onclick={applySkill} disabled={!selectedSkill || !isInvocable}>应用并发送</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: var(--overlay);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-modal);
    animation: fadeIn var(--duration-fast) var(--ease-out);
  }

  @keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }

  .modal-dialog {
    width: 480px;
    max-width: 90vw;
    max-height: 80vh;
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    overflow: hidden;
    display: flex;
    flex-direction: column;
    box-shadow: var(--shadow-xl);
    animation: slideUp var(--duration-normal) var(--ease-out);
  }

  .skill-use-dialog {
    width: 680px;
    max-width: 92vw;
    height: 480px;
    max-height: 70vh;
  }

  @keyframes slideUp { from { opacity: 0; transform: translateY(16px); } to { opacity: 1; transform: translateY(0); } }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-4);
    border-bottom: 1px solid var(--border);
  }

  .modal-header h3 {
    margin: 0;
    font-size: var(--text-md);
  }

  .modal-close {
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .modal-body {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .modal-footer {
    flex-shrink: 0;
    padding: var(--space-3) var(--space-4);
    border-top: 1px solid var(--border);
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }

  .modal-btn {
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface-1);
    color: var(--foreground);
    cursor: pointer;
  }

  .modal-btn.primary {
    background: var(--primary);
    border-color: var(--primary);
    color: white;
  }

  .modal-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .skill-use-layout {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 0;
    height: 100%;
    min-height: 0;
  }

  .skill-use-list {
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }

  .skill-use-search {
    flex-shrink: 0;
    padding: var(--space-3);
    border-bottom: 1px solid var(--border);
  }

  .skill-use-search input {
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    font-size: var(--text-sm);
  }

  .skill-use-items {
    flex: 1;
    min-height: 0;
    padding: var(--space-2);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .skill-use-item {
    flex-shrink: 0;
    text-align: left;
    padding: var(--space-2) var(--space-3);
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    cursor: pointer;
    color: var(--foreground);
    transition: all var(--transition-fast);
  }

  .skill-use-item:hover {
    background: var(--surface-hover);
    transform: translateX(2px);
  }

  .skill-use-item.active {
    border-color: var(--primary);
    background: var(--primary-muted);
  }

  .skill-use-name {
    font-weight: var(--font-medium);
    font-size: var(--text-sm);
    margin-bottom: 2px;
  }

  .skill-use-desc-row {
    overflow: hidden;
  }

  .skill-use-desc {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    line-height: 1.4;
  }

  .skill-use-detail {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }

  .skill-use-detail-scroll {
    flex: 1;
    min-height: 0;
    padding: var(--space-4);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .skill-use-detail-header {
    flex-shrink: 0;
  }

  .skill-use-title {
    font-size: var(--text-md);
    font-weight: var(--font-semibold);
    margin-bottom: var(--space-1);
  }

  .skill-use-meta {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    line-height: 1.4;
    margin-bottom: var(--space-2);
  }

  .skill-use-chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .skill-use-chip {
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: var(--surface-2);
    font-size: 10px;
    white-space: nowrap;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .skill-use-field label,
  .skill-use-label {
    display: block;
    font-size: var(--text-xs);
    margin-bottom: var(--space-1);
    color: var(--foreground-muted);
  }

  .skill-use-field textarea {
    width: 100%;
    min-height: 60px;
    max-height: 80px;
    resize: none;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    font-size: var(--text-sm);
  }

  .skill-use-preview {
    max-height: 95px;
    overflow-y: auto;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .skill-use-select {
    width: 100%;
    height: var(--btn-height-md);
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
  }

  .skill-use-empty {
    padding: var(--space-4);
    text-align: center;
    color: var(--foreground-muted);
  }

  .skill-use-warning {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--warning);
    border-radius: var(--radius-sm);
    color: var(--warning);
    font-size: var(--text-xs);
  }

  @media (max-width: 720px) {
    .skill-use-dialog {
      width: 95vw;
      height: 70vh;
    }
    .skill-use-layout {
      grid-template-columns: 1fr;
      grid-template-rows: 160px 1fr;
    }
    .skill-use-list {
      border-right: none;
      border-bottom: 1px solid var(--border);
    }
  }
</style>
