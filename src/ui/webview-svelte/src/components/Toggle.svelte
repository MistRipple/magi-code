<script lang="ts">
  /**
   * Toggle 开关组件
   * 使用原生 checkbox 实现，提供良好的可访问性
   */

  interface Props {
    checked?: boolean;
    disabled?: boolean;
    title?: string;
    onchange?: (checked: boolean) => void;
  }

  let {
    checked = false,
    disabled = false,
    title = '',
    onchange
  }: Props = $props();

  function handleChange(event: Event) {
    const target = event.target as HTMLInputElement;
    if (onchange) {
      onchange(target.checked);
    }
  }
</script>

<label class="toggle" class:disabled {title}>
  <input
    type="checkbox"
    {checked}
    {disabled}
    onchange={handleChange}
  />
  <span class="toggle-slider"></span>
</label>

<style>
  .toggle {
    position: relative;
    display: inline-block;
    width: 36px;
    height: 20px;
    cursor: pointer;
    flex-shrink: 0;
  }

  .toggle.disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  .toggle input {
    opacity: 0;
    width: 0;
    height: 0;
    position: absolute;
  }

  .toggle-slider {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: var(--surface-3, #3c3c3c);
    border: 1px solid var(--border, #454545);
    border-radius: var(--radius-full, 9999px);
    transition: all var(--transition-fast, 0.15s ease);
  }

  .toggle-slider::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    background: var(--foreground-muted, #999);
    border-radius: var(--radius-full, 9999px);
    transition: all var(--transition-fast, 0.15s ease);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
  }

  .toggle:hover .toggle-slider {
    border-color: var(--primary-muted, #4a9eff66);
  }

  /* 选中状态 */
  .toggle input:checked + .toggle-slider {
    background: var(--success, #3fb950);
    border-color: var(--success, #3fb950);
  }

  .toggle input:checked + .toggle-slider::after {
    left: 18px;
    background: #fff;
  }

  /* 焦点状态（键盘导航可访问性） */
  .toggle input:focus-visible + .toggle-slider {
    outline: 2px solid var(--primary, #4a9eff);
    outline-offset: 2px;
  }
</style>

