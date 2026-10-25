<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import FileTypeIcon from '../components/FileTypeIcon.svelte';
  import { fileTypeCatalog } from '../lib/file-type';
  import { icons, type IconName } from '../lib/icons';
  import { initializeAppearanceRuntime, getAppearanceRuntimeSnapshot, previewAppearanceTheme, restoreActiveAppearance } from '../appearance/runtime';

  const seen = new Set<string>();
  const entries = (Object.entries(icons) as [IconName, string][]).filter(([, path]) => {
    if (seen.has(path)) return false;
    seen.add(path);
    return true;
  });
  const aliases = (name: IconName) => Object.keys(icons).filter(key => key !== name && icons[key as IconName] === icons[name]).join(' / ');
  const fileGroups = [
    ['code', '编程语言与框架'], ['web', '网页与样式'], ['config', '配置与工程'],
    ['data', '文本与数据'], ['office', '办公与阅读'], ['image', '图片与设计'],
    ['media', '音视频与字幕'], ['archive', '压缩包与磁盘映像'],
    ['system', '字体、程序与证书'], ['engineering', '模型与工程图'],
  ].map(([id, label]) => ({ id, label, files: fileTypeCatalog.filter(file => file.category === id) }));
  const extensionCount = fileTypeCatalog.reduce((count, file) => count + file.extensions.length, 0);
  const specialNameCount = fileTypeCatalog.reduce((count, file) => count + ('filenames' in file ? file.filenames.length : 0), 0);
  let search = $state('');
  let skin = $state('');
  let themes = $state<{ id: string; name: string }[]>([]);
  let busy = $state(false);
  let error = $state('');
  const filteredFileGroups = $derived(fileGroups.map(group => ({ ...group, files: group.files.filter(file =>
    [group.label, file.label, file.kind, file.example, ...file.extensions, ...('filenames' in file ? file.filenames : [])]
      .join(' ').toLowerCase().includes(search.trim().toLowerCase()),
  ) })).filter(group => group.files.length));
  const filtered = $derived(entries.filter(([name]) => `${name} ${aliases(name)}`.includes(search.trim().toLowerCase())));

  onMount(() => {
    void initializeAppearanceRuntime().then(() => {
      const { library } = getAppearanceRuntimeSnapshot();
      themes = library?.themes.map(({ pack }) => ({ id: pack.id, name: pack.name })) ?? [];
      skin = library?.activeThemeId ?? '';
    }).catch(() => error = '暂时无法读取皮肤');
  });
  async function changeSkin(id: string) {
    const pack = getAppearanceRuntimeSnapshot().library?.themes.find(({ pack }) => pack.id === id)?.pack;
    if (!pack || busy) return;
    busy = true;
    try { await previewAppearanceTheme(pack); skin = id; error = ''; }
    catch { error = '皮肤加载失败'; }
    finally { busy = false; }
  }
  async function restore() {
    busy = true;
    try { await restoreActiveAppearance(); skin = getAppearanceRuntimeSnapshot().library?.activeThemeId ?? ''; error = ''; }
    catch { error = '皮肤恢复失败'; }
    finally { busy = false; }
  }
</script>

<main class="icon-library">
  <header>
    <div><a class="back-link" href="./product-preview.html"><Icon name="chevron-left" size={12}/>返回工作台 Demo</a><h1>Magi 图标库</h1><p>一套线性图形 · 统一圆头、圆角与线宽 · 颜色随皮肤变化</p></div>
    <div class="controls"><select aria-label="预览皮肤" disabled={busy || !themes.length} value={skin} onchange={event => changeSkin(event.currentTarget.value)}>{#each themes as theme}<option value={theme.id}>{theme.name}</option>{/each}</select><button disabled={busy} onclick={restore}>当前皮肤</button></div>
  </header>
  <div class="library-toolbar"><span>{entries.length} 个图形 · {Object.keys(icons).length} 个语义名称</span><input aria-label="搜索图标" placeholder="搜索图标名称…" bind:value={search}/></div>
  {#if error}<p role="status">{error}</p>{/if}
  <div class="samples" aria-label="应用场景">
    <span><Icon name="chat"/>会话</span><span><Icon name="settings"/>设置</span><span><Icon name="terminal"/>终端</span><span><Icon name="plug"/>MCP</span><span><Icon name="bot"/>代理</span><span><Icon name="file-code"/>代码文件</span><span><Icon name="copy"/>复制</span><span><Icon name="chevron-right"/>展开</span>
  </div>
  <section id="file-types" aria-label="文件类型图标">
    <h2>文件类型</h2><p>{fileTypeCatalog.length} 类标识 · {extensionCount} 个后缀 · {specialNameCount} 个特殊文件名。类型识别色在不同皮肤中保留，浅色与深色自动补偿。</p>
    {#each filteredFileGroups as group}
      <section class="file-group" aria-label={group.label}>
        <h3>{group.label}</h3>
        <div class="file-type-examples">
          {#each group.files as file}
            <article>
              <div class="file-identity"><FileTypeIcon path={file.example} size={20}/><code>{file.example}</code></div>
              <small>{file.label}</small>
              <div class="file-variants">
                {#each file.extensions as extension}<span title={`example.${extension}`}><FileTypeIcon path={`example.${extension}`} size={14}/><code>.{extension}</code></span>{/each}
                {#if 'filenames' in file}{#each file.filenames as name}<span title={name}><FileTypeIcon path={name} size={14}/><code>{name}</code></span>{/each}{/if}
              </div>
            </article>
          {/each}
        </div>
      </section>
    {/each}
    <div class="file-fallback"><FileTypeIcon path="unknown.custom" size={16}/><code>unknown.custom</code><small>未知类型使用中性文件图标</small></div>
  </section>
  <h2>界面图标</h2>
  <div class="icon-grid">
    {#each filtered as [name]}
      <article title={aliases(name) ? `${name} / ${aliases(name)}` : name}>
        <div class="sizes"><Icon {name} size={14}/><Icon {name} size={16}/><Icon {name} size={20}/></div>
        <code>{name}</code><small>{aliases(name) || '14 / 16 / 20 px'}</small>
      </article>
    {/each}
  </div>
  {#if !filtered.length && !filteredFileGroups.length}<p>没有匹配的图标。</p>{/if}
  <footer>与正式工作台共用 Icon.svelte 和 icons.ts。状态仍使用独立的红 / 绿 / 蓝圆点。<a href="/assets/licenses/lucide.txt">Lucide 1.61.0 · ISC / MIT</a></footer>
</main>

<style>
  .icon-library { max-width:1160px; margin:0 auto; padding:36px 28px; font:13px/1.6 var(--font-family,system-ui); color:var(--ink); }
  header { display:flex; justify-content:space-between; gap:24px; align-items:center; margin-bottom:26px; }
  h1 { font-size:23px; margin:12px 0 6px; font-weight:var(--font-semibold,600); }
  p, small, footer { color:var(--muted); }
  a { color:var(--muted); font-size:12px; }
  a:hover { color:var(--ink); }
  .back-link { display:inline-flex; align-items:center; gap:3px; }
  .controls { display:flex; gap:8px; }
  select, button, input { color:var(--ink); border:0; border-radius:6px; background:var(--code); padding:7px 10px; font:inherit; }
  select option { color:var(--ink); background:var(--bg); }
  button:hover { background:var(--hover); }
  :is(button,input,select):focus-visible { outline:0; box-shadow:inset 0 0 0 1px var(--accent); }
  .library-toolbar { display:flex; align-items:center; justify-content:space-between; gap:20px; color:var(--muted); margin-bottom:20px; }
  .samples { display:flex; flex-wrap:wrap; gap:22px; padding:18px 0; margin-bottom:20px; box-shadow:0 1px 0 var(--line); }
  .samples span { display:inline-flex; align-items:center; gap:7px; }
  .icon-grid { display:grid; grid-template-columns:repeat(auto-fill,minmax(155px,1fr)); gap:10px; }
  article { display:flex; flex-direction:column; align-items:center; justify-content:center; padding:20px 10px 16px; border-radius:9px; background:var(--card); min-width:0; }
  .sizes { display:flex; align-items:center; justify-content:center; gap:19px; height:32px; margin-bottom:10px; }
  code { font:12px/1.5 var(--font-mono,monospace); }
  small { font-size:11px; text-align:center; min-height:18px; overflow-wrap:anywhere; }
  h2 { font-size:15px; margin:26px 0 12px; font-weight:var(--font-semibold,600); }
  #file-types { scroll-margin-top:20px; }
  #file-types > p { font-size:12px; margin-bottom:12px; }
  h3 { font-size:13px; font-weight:var(--font-medium,500); margin:24px 0 10px; color:var(--muted); }
  .file-type-examples { display:grid; grid-template-columns:repeat(auto-fill,minmax(min(100%,250px),1fr)); gap:10px; align-items:start; }
  .file-type-examples article { align-items:stretch; justify-content:flex-start; gap:8px; padding:14px; }
  .file-type-examples small { text-align:left; min-height:0; }
  .file-identity, .file-fallback { display:flex; align-items:center; gap:10px; min-width:0; }
  .file-identity code { overflow-wrap:anywhere; }
  .file-variants { display:flex; flex-wrap:wrap; gap:8px 12px; margin-top:4px; }
  .file-variants span { display:inline-flex; align-items:center; gap:4px; min-width:0; }
  .file-variants code { font-size:11px; color:var(--muted); overflow-wrap:anywhere; }
  .file-fallback { flex-wrap:wrap; margin-top:18px; }
  .file-fallback small { min-height:0; }
  footer { margin-top:30px; font-size:12px; display:flex; justify-content:space-between; gap:20px; }
  @media(max-width:650px) { header { align-items:flex-start; flex-direction:column; } .icon-library { padding:24px 16px; } .library-toolbar { flex-wrap:wrap; } footer { flex-direction:column; } }
</style>
