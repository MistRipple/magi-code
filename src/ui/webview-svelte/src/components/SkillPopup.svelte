<script lang="ts">
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';

  interface Skill {
    id: string;
    name: string;
    description: string;
    author?: string;
    tags?: string[];
    template?: string;
  }

  interface Props {
    visible: boolean;
    onClose: () => void;
  }

  let { visible, onClose }: Props = $props();

  // 技能列表
  let skills = $state<Skill[]>([]);
  let searchQuery = $state('');
  let selectedSkill = $state<Skill | null>(null);
  let userInput = $state('');

  // 过滤后的技能列表
  const filteredSkills = $derived(() => {
    if (!searchQuery.trim()) return skills;
    const query = searchQuery.toLowerCase();
    return skills.filter(s => 
      s.name.toLowerCase().includes(query) || 
      s.description.toLowerCase().includes(query)
    );
  });

  // 选择技能
  function selectSkill(skill: Skill) {
    selectedSkill = skill;
    userInput = '';
  }

  // 使用技能
  function useSkill() {
    if (!selectedSkill) return;
    vscode.postMessage({
      type: 'useSkill',
      skillId: selectedSkill.id,
      input: userInput,
    });
    onClose();
  }

  // 关闭弹窗
  function handleOverlayClick() {
    onClose();
  }

  // 生成预览
  const preview = $derived(() => {
    if (!selectedSkill?.template) return '';
    return selectedSkill.template.replace('{{input}}', userInput || '...');
  });

  // 监听技能列表更新
  $effect(() => {
    if (visible) {
      vscode.postMessage({ type: 'getSkills' });
    }
  });

  // 接收技能列表
  $effect(() => {
    const handler = (event: MessageEvent) => {
      const msg = event.data;
      if (msg.type === 'skillsList' && msg.skills) {
        skills = msg.skills;
      }
    };
    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  });
</script>

{#if visible}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={handleOverlayClick} onkeydown={(e) => e.key === 'Escape' && onClose()} role="presentation">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal-dialog skill-use-dialog" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
      <div class="modal-header">
        <h3>使用 Skill</h3>
        <button class="modal-close" onclick={onClose} title="关闭">
          <Icon name="close" size={14} />
        </button>
      </div>
      <div class="modal-body">
        <div class="skill-use-layout">
          <!-- 左侧：技能列表 -->
          <div class="skill-use-list">
            <div class="skill-use-search">
              <input type="text" bind:value={searchQuery} placeholder="搜索技能...">
            </div>
            <div class="skill-use-items">
              {#if filteredSkills().length === 0}
                <div class="skill-use-empty">暂无可用技能</div>
              {:else}
                {#each filteredSkills() as skill (skill.id)}
                  <button
                    type="button"
                    class="skill-use-item"
                    class:active={selectedSkill?.id === skill.id}
                    onclick={() => selectSkill(skill)}
                  >
                    <div class="skill-use-name">{skill.name}</div>
                    <div class="skill-use-desc-row">
                      <span class="skill-use-desc">{skill.description}</span>
                    </div>
                  </button>
                {/each}
              {/if}
            </div>
          </div>

          <!-- 右侧：技能详情 -->
          <div class="skill-use-detail">
            {#if selectedSkill}
              <div class="skill-use-detail-scroll">
                <div class="skill-use-detail-header">
                  <div class="skill-use-title">{selectedSkill.name}</div>
                  {#if selectedSkill.author}
                    <div class="skill-use-meta">作者: {selectedSkill.author}</div>
                  {/if}
                  {#if selectedSkill.tags?.length}
                    <div class="skill-use-chips">
                      {#each selectedSkill.tags as tag}
                        <span class="skill-use-chip">{tag}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
                <div class="skill-use-field">
                  <label for="skill-user-input">输入内容</label>
                  <textarea id="skill-user-input" bind:value={userInput} placeholder="输入你的需求..."></textarea>
                </div>
                {#if preview()}
                  <div class="skill-use-preview">{preview()}</div>
                {/if}
              </div>
            {:else}
              <div class="skill-use-empty">请从左侧选择一个技能</div>
            {/if}
          </div>
        </div>
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" onclick={onClose}>取消</button>
        <button class="modal-btn primary" onclick={useSkill} disabled={!selectedSkill}>
          使用技能
        </button>
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
    width: 720px;
    max-width: 92vw;
  }

  @keyframes slideUp { from { opacity: 0; transform: translateY(16px); } to { opacity: 1; transform: translateY(0); } }

  .modal-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-4);
    border-bottom: 1px solid var(--border);
  }

  .modal-header h3 {
    font-size: var(--text-lg);
    font-weight: var(--font-semibold);
    margin: 0;
    color: var(--foreground);
  }

  .modal-close {
    width: var(--btn-height-md);
    height: var(--btn-height-md);
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .modal-close:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .modal-body {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-4);
  }

  .modal-footer {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-3);
    padding: var(--space-4);
    border-top: 1px solid var(--border);
  }

  .modal-btn {
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    border-radius: var(--radius-sm);
    border: none;
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .modal-btn.secondary {
    background: var(--secondary);
    color: var(--foreground);
  }

  .modal-btn.secondary:hover {
    background: var(--secondary-hover);
  }

  .modal-btn.primary {
    background: var(--primary);
    color: white;
  }

  .modal-btn.primary:hover {
    background: var(--primary-hover);
  }

  .modal-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .skill-use-layout {
    display: grid;
    grid-template-columns: 240px 1fr;
    gap: var(--space-4);
    align-items: stretch;
  }

  .skill-use-list {
    border-right: 1px solid var(--border);
    padding-right: var(--space-4);
    display: flex;
    flex-direction: column;
    height: 360px;
  }

  .skill-use-search input {
    width: 100%;
    height: var(--btn-height-lg);
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    background: var(--vscode-input-background, #3c3c3c);
    color: var(--foreground);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    outline: none;
  }

  .skill-use-search input:focus {
    border-color: var(--primary);
  }

  .skill-use-items {
    margin-top: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    overflow-y: auto;
    flex: 1;
  }

  .skill-use-item {
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    cursor: pointer;
    border: 1px solid transparent;
    transition: all var(--transition-fast);
  }

  .skill-use-item:hover {
    border-color: var(--primary);
    background: var(--surface-hover);
  }

  .skill-use-item.active {
    border-color: var(--primary);
    background: var(--surface-selected);
  }

  .skill-use-name {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .skill-use-desc-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-top: var(--space-1);
  }

  .skill-use-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: var(--leading-normal);
    display: -webkit-box;
    -webkit-line-clamp: 1;
    -webkit-box-orient: vertical;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .skill-use-detail {
    display: flex;
    flex-direction: column;
    height: 360px;
    overflow: hidden;
  }

  .skill-use-detail-scroll {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    height: 100%;
    overflow-y: auto;
    padding-right: var(--space-1);
  }

  .skill-use-empty {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    padding: var(--space-5) 0;
    text-align: center;
  }

  .skill-use-detail-header {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .skill-use-title {
    font-size: var(--text-lg);
    font-weight: var(--font-bold);
    color: var(--foreground);
  }

  .skill-use-meta {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
  }

  .skill-use-chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }

  .skill-use-chip {
    font-size: var(--text-xs);
    padding: var(--space-1) var(--space-3);
    border-radius: var(--radius-full);
    background: var(--primary-muted);
    color: var(--primary);
  }

  .skill-use-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .skill-use-field label {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
  }

  .skill-use-field textarea {
    width: 100%;
    min-height: 120px;
    resize: vertical;
    padding: var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--vscode-input-background, #3c3c3c);
    color: var(--foreground);
    font-size: var(--text-sm);
    outline: none;
    font-family: inherit;
    line-height: var(--leading-normal);
  }

  .skill-use-field textarea:focus {
    border-color: var(--primary);
  }

  .skill-use-preview {
    padding: var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    white-space: pre-wrap;
    line-height: var(--leading-normal);
    max-height: 160px;
    overflow: auto;
    font-family: var(--font-mono);
  }

  @media (max-width: 720px) {
    .skill-use-layout {
      grid-template-columns: 1fr;
    }
    .skill-use-list {
      border-right: none;
      padding-right: 0;
      border-bottom: 1px solid var(--border);
      padding-bottom: var(--space-4);
      height: auto;
      max-height: 200px;
    }
    .skill-use-detail {
      height: auto;
    }
  }
</style>

