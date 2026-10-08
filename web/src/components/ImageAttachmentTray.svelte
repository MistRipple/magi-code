<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { formatImageSize, MAX_COMPOSER_IMAGES } from '../lib/composer-policy';
  import Icon from './Icon.svelte';

  interface TrayImage {
    id: string;
    dataUrl: string;
    name: string;
    size?: number;
  }

  interface Props {
    images: TrayImage[];
    /** 正在读取、尚未出现在 images 里的张数，用来给用户占位反馈。 */
    pendingCount?: number;
    disabled?: boolean;
    onRemove: (imageId: string) => void;
    onClear: () => void;
  }

  let { images, pendingCount = 0, disabled = false, onRemove, onClear }: Props = $props();
  let previewIndex = $state<number | null>(null);

  // 输入区是 container-type 容器，会把 position: fixed 的后代限制在自己内部；
  // 预览必须挂到 body 才能铺满整个窗口。
  function portalToBody(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      },
    };
  }

  const total = $derived(images.length + pendingCount);
  const atLimit = $derived(total >= MAX_COMPOSER_IMAGES);

  function describe(image: TrayImage, index: number): string {
    const size = image.size ? formatImageSize(image.size) : '';
    const base = image.name || i18n.t('messageItem.imageAlt', { index: index + 1 });
    return size ? `${base} · ${size}` : base;
  }

  function openPreview(index: number): void {
    previewIndex = index;
  }

  function closePreview(): void {
    previewIndex = null;
  }

  function step(delta: number): void {
    if (previewIndex === null || images.length === 0) return;
    previewIndex = (previewIndex + delta + images.length) % images.length;
  }

  function handlePreviewKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      closePreview();
    } else if (event.key === 'ArrowRight') {
      event.preventDefault();
      step(1);
    } else if (event.key === 'ArrowLeft') {
      event.preventDefault();
      step(-1);
    }
  }

  // 预览中的图片被删除或清空后，不能留着悬空的下标。
  $effect(() => {
    if (previewIndex !== null && previewIndex >= images.length) {
      previewIndex = images.length === 0 ? null : images.length - 1;
    }
  });

  $effect(() => {
    if (previewIndex === null) return;
    window.addEventListener('keydown', handlePreviewKeydown, true);
    return () => window.removeEventListener('keydown', handlePreviewKeydown, true);
  });
</script>

<div class="image-tray" role="group" aria-label={i18n.t('input.imageTray.label')}>
  <div class="image-tray-list">
    {#each images as image, index (image.id)}
      <div class="image-tray-item">
        <button
          type="button"
          class="image-tray-thumb"
          onclick={() => openPreview(index)}
          title={describe(image, index)}
          aria-label={i18n.t('input.imageTray.preview', { name: describe(image, index) })}
        >
          <img src={image.dataUrl} alt={image.name} />
        </button>
        <button
          type="button"
          class="image-tray-remove"
          onclick={() => onRemove(image.id)}
          {disabled}
          title={i18n.t('input.remove')}
          aria-label={i18n.t('input.imageTray.removeNamed', { name: image.name || String(index + 1) })}
        >
          <Icon name="close" size={10} />
        </button>
      </div>
    {/each}
    {#each Array.from({ length: pendingCount }) as _, index (index)}
      <div class="image-tray-item image-tray-pending" aria-hidden="true"></div>
    {/each}
  </div>
  <div class="image-tray-meta">
    <span class="image-tray-count" class:at-limit={atLimit}>
      {i18n.t('input.imageTray.count', { count: total, max: MAX_COMPOSER_IMAGES })}
    </span>
    {#if images.length > 1}
      <button type="button" class="image-tray-clear" onclick={onClear} {disabled}>
        {i18n.t('input.clearImages')}
      </button>
    {/if}
  </div>
</div>

{#if previewIndex !== null && images[previewIndex]}
  {@const current = images[previewIndex]}
  <div class="image-lightbox" use:portalToBody role="dialog" aria-modal="true" aria-label={i18n.t('messageItem.imagePreviewAlt')}>
    <button
      type="button"
      class="image-lightbox-backdrop"
      onclick={closePreview}
      aria-label={i18n.t('messageItem.imagePreviewClose')}
    ></button>
    <figure class="image-lightbox-content">
      <img src={current.dataUrl} alt={current.name} />
      <figcaption>
        {describe(current, previewIndex)}
        {#if images.length > 1}
          <span class="image-lightbox-position">{previewIndex + 1} / {images.length}</span>
        {/if}
      </figcaption>
    </figure>
    {#if images.length > 1}
      <button type="button" class="image-lightbox-nav prev" onclick={() => step(-1)} aria-label={i18n.t('input.imageTray.previous')}>
        <Icon name="chevron-left" size={18} />
      </button>
      <button type="button" class="image-lightbox-nav next" onclick={() => step(1)} aria-label={i18n.t('input.imageTray.next')}>
        <Icon name="chevron-right" size={18} />
      </button>
    {/if}
    <button type="button" class="image-lightbox-close" onclick={closePreview} aria-label={i18n.t('messageItem.imagePreviewClose')}>
      <Icon name="close" size={14} />
    </button>
  </div>
{/if}

<style>
  .image-tray {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    /* 顶部留出删除按钮探出的 6px，否则会被容器边缘裁掉。 */
    padding: 12px var(--space-3) 0;
  }

  .image-tray-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    min-width: 0;
  }

  .image-tray-item {
    position: relative;
    width: 64px;
    height: 64px;
    flex: 0 0 auto;
  }

  .image-tray-thumb {
    display: block;
    width: 100%;
    height: 100%;
    padding: 0;
    overflow: hidden;
    cursor: zoom-in;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    transition: border-color var(--transition-fast), transform var(--transition-fast);
  }

  .image-tray-thumb:hover,
  .image-tray-thumb:focus-visible {
    border-color: color-mix(in srgb, var(--primary) 55%, var(--border));
    outline: none;
  }

  .image-tray-thumb img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  /* 删除按钮常显：触屏和键盘都够得到，不依赖悬停。 */
  .image-tray-remove {
    position: absolute;
    top: -6px;
    right: -6px;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding: 0;
    color: var(--foreground);
    cursor: pointer;
    background: var(--surface-3, var(--surface-2));
    border: 1px solid var(--border);
    border-radius: 50%;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.35);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .image-tray-remove:hover:not(:disabled),
  .image-tray-remove:focus-visible {
    color: white;
    background: var(--destructive);
    border-color: var(--destructive);
    outline: none;
  }

  .image-tray-remove:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .image-tray-pending {
    background: var(--surface-2);
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    animation: imageTrayPulse 1.2s ease-in-out infinite;
  }

  .image-tray-meta {
    display: flex;
    flex: 0 0 auto;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .image-tray-count {
    font-variant-numeric: tabular-nums;
  }

  .image-tray-count.at-limit {
    color: var(--warning);
  }

  .image-tray-clear {
    padding: 0;
    color: var(--foreground-muted);
    cursor: pointer;
    background: transparent;
    border: none;
    font-size: inherit;
  }

  .image-tray-clear:hover:not(:disabled) {
    color: var(--destructive);
  }

  .image-lightbox {
    position: fixed;
    inset: 0;
    z-index: var(--z-modal);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .image-lightbox-backdrop {
    position: absolute;
    inset: 0;
    padding: 0;
    cursor: zoom-out;
    background: rgba(0, 0, 0, 0.85);
    border: none;
  }

  .image-lightbox-content {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2);
    max-width: 90vw;
    max-height: 90vh;
    margin: 0;
    pointer-events: none;
  }

  .image-lightbox-content img {
    max-width: 90vw;
    max-height: calc(90vh - 32px);
    object-fit: contain;
    border-radius: var(--radius-sm);
  }

  .image-lightbox-content figcaption {
    display: flex;
    gap: var(--space-2);
    color: rgba(255, 255, 255, 0.85);
    font-size: var(--text-xs);
  }

  .image-lightbox-position {
    color: rgba(255, 255, 255, 0.6);
    font-variant-numeric: tabular-nums;
  }

  .image-lightbox-nav,
  .image-lightbox-close {
    position: absolute;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    padding: 0;
    color: white;
    cursor: pointer;
    background: rgba(255, 255, 255, 0.12);
    border: none;
    border-radius: 50%;
  }

  .image-lightbox-nav:hover,
  .image-lightbox-close:hover {
    background: rgba(255, 255, 255, 0.24);
  }

  .image-lightbox-nav.prev { left: 24px; top: 50%; transform: translateY(-50%); }
  .image-lightbox-nav.next { right: 24px; top: 50%; transform: translateY(-50%); }
  .image-lightbox-close { top: 20px; right: 20px; }

  @keyframes imageTrayPulse {
    0%, 100% { opacity: 0.45; }
    50% { opacity: 0.9; }
  }
</style>
