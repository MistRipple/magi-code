<script lang="ts">
  import Icon from './Icon.svelte';
  import MagiWordmark from './MagiWordmark.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { vscode } from '../lib/vscode-bridge';

  const REPOSITORY_URL = 'https://github.com/MistRipple/magi-code';

  function openRepository(path = ''): void {
    vscode.postMessage({ type: 'openLink', url: `${REPOSITORY_URL}${path}` });
  }
</script>

<div class="settings-tab-inner project-tab">
  <div class="project-content">
    <section class="project-introduction" aria-label={i18n.t('settings.project.title')}>
      <MagiWordmark width={134} height={39} />
      <p>{i18n.t('settings.project.intro')}</p>
    </section>

    <section class="repository-card" aria-labelledby="repository-title">
      <div class="repository-heading">
        <div class="repository-symbol" aria-hidden="true">
          <Icon name="git-branch" size={17} />
        </div>
        <div>
          <span class="repository-eyebrow">{i18n.t('settings.project.repositoryLabel')}</span>
          <h3 id="repository-title">MistRipple / magi-code</h3>
        </div>
      </div>

      <p class="repository-description">{i18n.t('settings.project.repositoryDesc')}</p>

      <code class="repository-url">github.com/MistRipple/magi-code</code>

      <div class="repository-actions">
        <button class="repository-open" type="button" onclick={() => openRepository()}>
          <span>{i18n.t('settings.project.openRepository')}</span>
          <Icon name="external-link" size={13} />
        </button>
      </div>

      <div class="repository-links" aria-label={i18n.t('settings.project.moreLinks')}>
        <button type="button" onclick={() => openRepository('/issues')}>
          {i18n.t('settings.project.issues')}
          <Icon name="external-link" size={11} />
        </button>
        <button type="button" onclick={() => openRepository('/releases')}>
          {i18n.t('settings.project.releases')}
          <Icon name="external-link" size={11} />
        </button>
      </div>
    </section>
  </div>
</div>

<style>
  .project-tab {
    box-sizing: border-box;
    padding: clamp(28px, 6vh, 72px) clamp(28px, 6vw, 72px);
  }

  .project-content {
    width: min(100%, 620px);
    margin: 0 auto;
  }

  .project-introduction {
    padding: 0 2px 30px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .project-introduction :global(.magi-wordmark) {
    color: var(--ind-foreground);
  }

  .project-introduction p {
    max-width: 460px;
    margin: 15px 0 0;
    color: var(--ind-foreground-secondary);
    font-size: 13px;
    line-height: 1.7;
    text-wrap: pretty;
  }

  .repository-card {
    margin-top: 28px;
    padding: 22px;
    border: 1px solid var(--ind-border-card);
    border-radius: var(--ind-radius-card);
    background: var(--ind-bg-card);
    box-shadow: var(--ind-shadow-sm);
  }

  .repository-heading {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .repository-symbol {
    display: grid;
    width: 32px;
    height: 32px;
    place-items: center;
    border: 1px solid var(--ind-border-control);
    border-radius: 9px;
    color: var(--ind-tab-accent);
    background: var(--ind-bg-control);
  }

  .repository-eyebrow {
    display: block;
    margin-bottom: 2px;
    color: var(--ind-foreground-muted);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.045em;
    text-transform: uppercase;
  }

  .repository-heading h3 {
    margin: 0;
    color: var(--ind-foreground);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 14px;
    font-weight: 600;
    letter-spacing: -0.02em;
  }

  .repository-description {
    margin: 18px 0 13px;
    color: var(--ind-foreground-secondary);
    font-size: 13px;
    line-height: 1.65;
    text-wrap: pretty;
  }

  .repository-url {
    display: block;
    overflow: hidden;
    padding: 9px 10px;
    border: 1px solid var(--ind-border-control);
    border-radius: 7px;
    color: var(--ind-foreground-secondary);
    background: var(--ind-bg-control);
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 11.5px;
    line-height: 1.35;
    text-overflow: ellipsis;
    user-select: text;
    white-space: nowrap;
  }

  .repository-actions {
    display: flex;
    align-items: center;
    gap: 9px;
    margin-top: 16px;
  }

  .repository-open,
  .repository-links button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 0;
    font: inherit;
    cursor: pointer;
    transition: color 140ms ease, border-color 140ms ease, background 140ms ease, transform 140ms ease;
  }

  .repository-open {
    min-height: 31px;
    gap: 7px;
    padding: 0 12px;
    border-radius: 7px;
    color: var(--background, #fff);
    background: var(--ind-tab-accent);
    font-size: 12px;
    font-weight: 600;
  }

  .repository-open:hover,
  .repository-links button:hover {
    transform: translateY(-1px);
  }

  .repository-open:hover {
    background: color-mix(in srgb, var(--ind-tab-accent) 88%, black);
  }

  .repository-links {
    display: flex;
    gap: 16px;
    margin-top: 20px;
    padding-top: 15px;
    border-top: 1px solid var(--ind-border-separator);
  }

  .repository-links button {
    gap: 5px;
    padding: 0;
    color: var(--ind-foreground-muted);
    background: transparent;
    font-size: 11.5px;
  }

  .repository-links button:hover {
    color: var(--ind-tab-accent);
  }

  .repository-open:focus-visible,
  .repository-links button:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--ind-tab-accent) 55%, transparent);
    outline-offset: 2px;
  }

  @media (max-width: 520px) {
    .project-tab { padding: 28px 20px; }
    .repository-card { padding: 18px; }
    .repository-actions { align-items: stretch; flex-direction: column; }
    .repository-open { width: 100%; }
  }
</style>
