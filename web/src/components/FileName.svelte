<script lang="ts">
  import { splitFileName } from '../lib/file-name';

  let { name, preserveExtension = true }: { name: string; preserveExtension?: boolean } = $props();
  const parts = $derived(preserveExtension ? splitFileName(name) : { stem: name, extension: '' });
</script>

<span class="file-name" title={name}>
  <span class="file-name-stem">{parts.stem}</span><span class="file-name-extension">{parts.extension}</span>
</span>

<style>
  .file-name {
    display: inline-flex;
    min-width: 0;
    max-width: 100%;
    vertical-align: bottom;
    white-space: nowrap;
  }

  .file-name-stem {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .file-name-extension {
    flex: 0 0 auto;
  }
</style>
