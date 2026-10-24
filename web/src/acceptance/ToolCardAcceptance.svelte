<script lang="ts">
  import { onMount } from 'svelte';
  import MessageItem from '../components/MessageItem.svelte';
  import ConversationTurn from '../components/ConversationTurn.svelte';
  import { initializeAppearanceRuntime, getAppearanceRuntimeSnapshot, previewAppearanceTheme } from '../appearance/runtime';
  import { i18n } from '../stores/i18n.svelte';
  import { acceptanceItems, call } from './tool-card-fixtures';
  const params = new URLSearchParams(location.search);
  const scope = { sessionId:params.get('sessionId') || 'acceptance-session', workspaceId:params.get('workspaceId') || undefined, workspacePath:params.get('workspacePath') || undefined };
  let mode = $state('original');
  let width = $state('820');
  let skin = $state('');
  let skins = $state<{id:string;name:string}[]>([]);
  let items = $state(acceptanceItems.filter(item => scope.workspaceId || item.key !== 'image-ready'));
  let active = $state(false);
  onMount(() => { void initializeAppearanceRuntime().then(() => { const {library,activeTheme} = getAppearanceRuntimeSnapshot(); skins=library?.themes.map(({pack})=>({id:pack.id,name:pack.name}))??[]; skin=activeTheme?.id??''; }); });
  async function theme(id:string) { const pack=getAppearanceRuntimeSnapshot().library?.themes.find(({pack})=>pack.id===id)?.pack; if(pack) await previewAppearanceTheme(pack); }
  function finish() {
    active=false;
    items=items.map(item=>item.key.startsWith('late-') ? {...item,message:{...item.message,blocks:item.message.blocks?.map(block=>block.toolCall ? {...block,toolCall:{...block.toolCall,status:'success',result:'新的输出已经接收完成。',durationMs:2456}} : block)}} : item);
  }
  function append() {
    active=true;
    const id=`late-${items.length}`;
    items=[...items.slice(0,-1),{...acceptanceItems[2],key:id,message:{...acceptanceItems[2].message,id,blocks:[call(id,'web_fetch','running',{url:'https://example.com/new-output'})]}},items[items.length-1]];
  }
</script>
<svelte:head><title>Magi 正式组件验收</title></svelte:head>
<header class="acceptance-toolbar"><strong>正式工具卡片验收</strong><select aria-label="显示模式" bind:value={mode}><option value="original">详细模式</option><option value="summary">摘要模式</option></select><select aria-label="内容宽度" bind:value={width}><option value="820">常规宽度</option><option value="420">420px</option><option value="320">320px</option></select><select aria-label="皮肤" bind:value={skin} onchange={e=>theme(e.currentTarget.value)}>{#each skins as theme}<option value={theme.id}>{theme.name}</option>{/each}</select><button onclick={append}>追加运行中的工具</button><button onclick={finish}>结束运行</button><button onclick={()=>i18n.setLocale(i18n.locale==='zh-CN'?'en-US':'zh-CN')}>中文 / English</button></header>
<main data-mode={mode} style:max-width={`${width}px`}>
  {#if mode==='summary'}<ConversationTurn turnId="card-acceptance" {items} runtimeActive={active} filePreviewScopeForItem={()=>scope} canEditMessage={()=>false} editMessage={()=>{}} />
  {:else}{#each items as item (item.key)}<MessageItem message={item.message} filePreviewScope={scope} />{/each}{/if}
</main>
<style>
  :global(body){margin:0;background:var(--magi-surface-main,var(--background));color:var(--foreground);}
  .acceptance-toolbar{position:sticky;top:0;z-index:10;display:flex;flex-wrap:wrap;gap:10px;align-items:center;padding:12px;background:var(--magi-surface-main,var(--background));box-shadow:inset 0 -1px var(--border-subtle);font-size:12px;}
  select,button{font:inherit;border:0;border-radius:5px;background:var(--surface-hover);color:var(--foreground);padding:6px;}
  main{width:100%;box-sizing:border-box;display:flex;flex-direction:column;gap:8px;max-width:820px;margin:auto;padding:20px 20px 80px;min-width:0;}
</style>
