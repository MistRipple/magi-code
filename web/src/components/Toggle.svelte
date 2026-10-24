<script lang="ts">
  /**
   * Toggle 开关组件
   * 使用原生 checkbox 实现，提供良好的可访问性
   */

  interface Props {
    checked?: boolean;
    disabled?: boolean;
    title?: string;
    ariaLabel?: string;
    size?: 'small' | 'default';
    onchange?: (checked: boolean) => void;
  }

  let {
    checked = false,
    disabled = false,
    title = '',
    ariaLabel = '',
    size = 'default',
    onchange
  }: Props = $props();

  function handleChange(event: Event) {
    const target = event.target as HTMLInputElement;
    if (onchange) {
      onchange(target.checked);
    }
  }
</script>

<label class="toggle" class:disabled class:small={size === 'small'} {title}>
  <input
    type="checkbox"
    role="switch"
    {checked}
    {disabled}
    aria-label={ariaLabel || title || undefined}
    onchange={handleChange}
  />
  <span class="toggle-slider control-switch" aria-hidden="true"></span>
</label>

<style>
  .toggle {
    --switch-width: 36px;
    --switch-height: 20px;
    --switch-inset: 3px;
    --switch-thumb: 14px;
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    vertical-align: middle;
    width: var(--switch-width);
    min-height: 24px;
    cursor: pointer;
    flex-shrink: 0;
  }

  .toggle.disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  .toggle input {
    opacity: 0;
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    padding: 0;
    border: 0;
    box-shadow: none;
    cursor: inherit;
  }

  .toggle-slider {
    pointer-events: none;
  }

  /* small 尺寸 */
  .toggle.small {
    --switch-width: 28px;
    --switch-height: 16px;
    --switch-inset: 2px;
    --switch-thumb: 12px;
  }
</style>
