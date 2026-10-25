<script lang="ts">
  import { onMount, onDestroy, untrack } from 'svelte';
  import Icon from '../components/Icon.svelte';

  interface Props { page: 'agents' | 'mcp'; onback: () => void; onfeedback: (text: string) => void; }
  let { page, onback, onfeedback }: Props = $props();
  type Entry = { id: string; name: string; detail: string; transport?: 'http' | 'stdio'; endpoint?: string; protocol?: string; };
  const draftKey = 'magi:design-demo:settings-drafts:v1';
  let agents = $state<Entry[]>([
    { id: 'review', name: '代码审阅', detail: '检查实现边界、可维护性与回归风险。' },
    { id: 'research', name: '资料研究', detail: '检索相关文档，整理带来源的研究结论。' },
  ]);
  let servers = $state<Entry[]>([
    { id: 'knowledge', name: '团队知识库', detail: '搜索与读取团队文档', transport: 'http', endpoint: 'https://knowledge.example/mcp', protocol: 'responses' },
    { id: 'files', name: '项目文件', detail: '访问授权目录中的文件', transport: 'stdio', endpoint: 'demo-files --root ./example', protocol: 'responses' },
  ]);
  let editor = $state<'agents' | 'mcp' | null>(null);
  let editingId = $state('');
  let name = $state('');
  let detail = $state('');
  let transport = $state<'http' | 'stdio'>('http');
  let endpoint = $state('https://knowledge.example/mcp');
  let httpEndpoint = $state('https://knowledge.example/mcp');
  let stdioCommand = $state('demo-files --root ./example');
  let protocol = $state('responses');
  let ready = $state(false);
  let restored = $state(false);
  let validation = $state('');
  let deleted = $state<{ page: 'agents' | 'mcp'; entry: Entry; index: number } | null>(null);
  let undoTimer: ReturnType<typeof setTimeout> | undefined;
  let drafts: Partial<Record<'agents' | 'mcp', { name: string; detail: string; transport: 'http' | 'stdio'; protocol: string }>> = {};
  let previousPage = $state(untrack(() => page));
  const entries = $derived(page === 'agents' ? agents : servers);
  const protocolPath = $derived(protocol === 'responses' ? '/v1/responses' : protocol === 'chat' ? '/v1/chat/completions' : '/v1/messages');

  onMount(() => {
    try {
      const value: unknown = JSON.parse(sessionStorage.getItem(draftKey) || '{}');
      if (value && typeof value === 'object') {
        for (const key of ['agents', 'mcp'] as const) {
          const candidate = (value as Record<string, unknown>)[key];
          if (candidate && typeof candidate === 'object') {
            const d = candidate as Record<string, unknown>;
            if (typeof d.name === 'string' && typeof d.detail === 'string') drafts[key] = { name: d.name.slice(0, 80), detail: d.detail.slice(0, 2000), transport: d.transport === 'stdio' ? 'stdio' : 'http', protocol: ['responses', 'chat', 'anthropic'].includes(String(d.protocol)) ? String(d.protocol) : 'responses' };
          }
        }
      }
    } catch { /* A disabled storage backend does not prevent the preview. */ }
    ready = true;
  });
  onDestroy(() => clearTimeout(undoTimer));
  $effect(() => {
    if (page !== previousPage) { editor = null; previousPage = page; validation = ''; }
  });
  $effect(() => {
    if (!ready || !editor || editingId) return;
    // Deliberately omit endpoint/command and credentials from persisted drafts.
    drafts[editor] = { name, detail, transport, protocol };
    try { sessionStorage.setItem(draftKey, JSON.stringify(drafts)); } catch { /* Draft stays in memory. */ }
  });

  function openEditor(entry?: Entry) {
    validation = '';
    editingId = entry?.id || '';
    const draft = entry ? undefined : drafts[page];
    restored = !!draft;
    name = entry?.name || draft?.name || '';
    detail = entry?.detail || draft?.detail || '';
    transport = entry?.transport || draft?.transport || 'http';
    endpoint = entry?.endpoint || 'https://knowledge.example/mcp';
    httpEndpoint = entry?.transport === 'http' && entry.endpoint ? entry.endpoint : 'https://knowledge.example/mcp';
    stdioCommand = entry?.transport === 'stdio' && entry.endpoint ? entry.endpoint : 'demo-files --root ./example';
    protocol = entry?.protocol || draft?.protocol || 'responses';
    editor = page;
  }
  function switchTransport(next: 'http' | 'stdio') {
    transport = next;
    endpoint = transport === 'http' ? httpEndpoint : stdioCommand;
  }
  function updateEndpoint(event: Event) {
    endpoint = (event.currentTarget as HTMLInputElement).value;
    if (transport === 'http') httpEndpoint = endpoint;
    else stdioCommand = endpoint;
  }
  function save(event: SubmitEvent) {
    event.preventDefault();
    if (!name.trim()) { validation = '请填写名称'; return; }
    const item: Entry = { id: editingId || `demo-${Date.now()}`, name: name.trim(), detail: detail.trim(), ...(page === 'mcp' ? { transport, endpoint, protocol } : {}) };
    const list = page === 'agents' ? agents : servers;
    const next = editingId ? list.map((entry) => entry.id === editingId ? item : entry) : [...list, item];
    if (page === 'agents') agents = next; else servers = next;
    if (!editingId) {
      delete drafts[page];
      try { sessionStorage.setItem(draftKey, JSON.stringify(drafts)); } catch { /* Optional storage. */ }
    }
    editor = null;
    onfeedback('演示配置已保存，仅在当前预览中生效');
  }
  function remove(entry: Entry) {
    clearTimeout(undoTimer);
    const index = entries.findIndex((item) => item.id === entry.id);
    deleted = { page, entry, index };
    if (page === 'agents') agents = agents.filter((item) => item.id !== entry.id);
    else servers = servers.filter((item) => item.id !== entry.id);
    undoTimer = setTimeout(() => deleted = null, 8000);
  }
  function undo() {
    if (!deleted) return;
    const { page: deletedPage, entry, index } = deleted;
    const list = [...(deletedPage === 'agents' ? agents : servers)];
    list.splice(index, 0, entry);
    if (deletedPage === 'agents') agents = list; else servers = list;
    deleted = null;
    clearTimeout(undoTimer);
    onfeedback('已撤销演示删除');
  }
</script>

<section class="settings-preview" aria-label={page === 'agents' ? '代理配置演示' : 'MCP 配置演示'}>
  <div class="content">
    <button class="back" onclick={() => editor ? editor = null : onback()}><Icon name="chevron-left" size={14} />{editor ? '返回列表' : '返回会话'}</button>
    <header>
      <div><span class="eyebrow">{page === 'agents' ? '代理' : '工具连接'} · 设计预览</span><h1>{editor ? editingId ? (page === 'agents' ? '编辑代理' : '编辑 MCP 服务') : (page === 'agents' ? '新增代理' : '新增 MCP 服务') : page === 'agents' ? '代理' : 'MCP 服务'}</h1><p>{editor ? '在当前内容区域完成配置，返回时保留新增草稿。' : page === 'agents' ? '为不同工作创建专长明确的代理。' : '连接外部工具，让 Magi 访问需要的资料与能力。'}</p></div>
      {#if !editor}<button class="primary" onclick={() => openEditor()}><Icon name="plus" size={14} />{page === 'agents' ? '新增代理' : '新增服务'}</button>{/if}
    </header>

    {#if editor}
      <form onsubmit={save}>
        {#if restored}<p class="hint restored"><Icon name="check" size={13} />已恢复此演示页的新增草稿</p>{/if}
        <label class="field"><span>名称</span><input bind:value={name} required maxlength="80" placeholder={page === 'agents' ? '例如：代码审阅' : '例如：团队知识库'} /></label>
        <label class="field"><span>{page === 'agents' ? '工作指引' : '说明'}</span><textarea bind:value={detail} rows={page === 'agents' ? 5 : 2} maxlength="2000" placeholder={page === 'agents' ? '描述代理的专长、工作方式和交付要求' : '简要描述这个服务提供的能力'}></textarea></label>
        {#if page === 'mcp'}
          <fieldset><legend>连接方式</legend><div class="options">
            <label><input type="radio" bind:group={transport} value="http" onchange={() => switchTransport('http')} />HTTP</label><label><input type="radio" bind:group={transport} value="stdio" onchange={() => switchTransport('stdio')} />stdio</label>
          </div><p class="hint">{transport === 'http' ? '使用服务提供的 HTTP 连接地址。' : '由本地命令启动服务，通过标准输入输出通信。'}</p></fieldset>
          <label class="field"><span>{transport === 'http' ? '服务地址' : '启动命令'}</span><input bind:value={endpoint} oninput={updateEndpoint} placeholder={transport === 'http' ? 'https://service.example/mcp' : 'demo-server --root ./example'} autocomplete="off" spellcheck="false" /><small>仅用于外观演示，不会连接或执行。请勿填写密钥；地址与命令不会写入草稿。</small></label>
          <div class="protocol-sample"><fieldset><legend>请求协议 <small>模型配置样例</small></legend><div class="options protocols">
            <label><input type="radio" bind:group={protocol} value="responses" />Responses API（推荐）</label><label><input type="radio" bind:group={protocol} value="chat" />Chat Completions</label><label><input type="radio" bind:group={protocol} value="anthropic" />Anthropic Messages</label>
          </div><p class="hint">实际请求 <code>POST {protocolPath}</code></p></fieldset><p class="hint">此处独立预览模型协议控件，不作为 MCP 的配置项。</p></div>
        {:else}
          <p class="hint">代理将在主会话分配任务时使用这些指引。此预览不会创建真实代理。</p>
        {/if}
        {#if validation}<p class="error" role="alert">{validation}</p>{/if}
        <footer><span class="hint">新增草稿仅保存在本次浏览器会话</span><button type="button" class="secondary" onclick={() => editor = null}>返回列表</button><button class="primary" type="submit">保存演示配置</button></footer>
      </form>
    {:else}
      <div class="entries">
        {#each entries as entry (entry.id)}
          <article class="entry"><span class="entry-icon"><Icon name={page === 'agents' ? 'profile' : 'tools'} size={18} /></span><div class="entry-copy"><h2>{entry.name}</h2><p>{entry.detail || '尚未添加说明'}</p>{#if page === 'mcp'}<small>{entry.transport === 'http' ? 'HTTP' : 'stdio'} · 演示连接</small>{/if}</div><div class="entry-actions"><button class="secondary" onclick={() => openEditor(entry)}>编辑</button><button class="icon-button" aria-label={`删除${entry.name}`} title={`删除${entry.name}`} onclick={() => remove(entry)}><Icon name="trash" size={14} /></button></div></article>
        {:else}<p class="empty">{page === 'agents' ? '还没有演示代理，创建一个开始预览。' : '还没有演示连接，添加服务查看配置效果。'}</p>{/each}
      </div>
      <p class="hint bottom-note">全部为演示配置。添加、修改和删除均不会影响真实的 Magi 设置。</p>
    {/if}
  </div>
  {#if deleted}<div class="undo-toast" role="status"><span>已删除「{deleted.entry.name}」</span><button onclick={undo}>撤销</button><button class="icon-button" aria-label="关闭删除提示" onclick={() => deleted = null}><Icon name="close" size={13} /></button></div>{/if}
</section>

<style>
  .settings-preview{position:relative;flex:1;min-height:0;min-width:0;overflow:auto;color:var(--ink,var(--foreground));scrollbar-gutter:stable}.content{max-width:880px;margin:auto;padding:30px 28px 60px}.back{display:inline-flex;align-items:center;gap:6px;color:var(--muted,var(--foreground-muted));font-size:12px;padding:4px 0;margin-bottom:24px}.back:hover{color:var(--ink,var(--foreground))}header{display:flex;align-items:center;justify-content:space-between;gap:20px;margin-bottom:26px}.eyebrow{color:var(--dim,var(--foreground-muted));font-size:12px}h1{margin:5px 0 7px;font-size:22px;font-weight:var(--bold,var(--font-semibold));letter-spacing:-.4px}h2{margin:0;font-size:13px;font-weight:var(--medium,var(--font-medium))}p{margin:0;color:var(--muted,var(--foreground-muted));line-height:1.75;text-wrap:pretty}header p{font-size:13px}button{border:0;background:transparent;color:inherit;font:inherit;cursor:pointer}input,textarea{font:inherit;color:var(--ink,var(--foreground))}.primary,.secondary{display:inline-flex;align-items:center;justify-content:center;gap:7px;flex:none;min-height:32px;padding:6px 11px;border-radius:7px;font-size:12px;white-space:nowrap}.primary{background:var(--accent,var(--primary));color:var(--primary-foreground,var(--bg));font-weight:var(--medium,var(--font-medium))}.secondary{color:var(--muted,var(--foreground-muted))}.secondary:hover,.icon-button:hover{background:var(--hover,var(--surface-hover));color:var(--ink,var(--foreground))}.entries{border-radius:10px;box-shadow:inset 0 0 0 var(--hairline,1px) var(--edge,var(--border));padding:0 16px}.entry{display:flex;align-items:center;gap:13px;min-width:0;padding:19px 0}.entry+.entry{box-shadow:inset 0 var(--hairline,1px) var(--line,var(--border-subtle))}.entry-icon{color:var(--muted,var(--foreground-muted));flex:none}.entry-copy{flex:1;min-width:0}.entry-copy p{font-size:12px;margin-top:3px;overflow-wrap:anywhere}.entry-copy small{display:block;font-size:12px;color:var(--dim,var(--foreground-muted));margin-top:5px}.entry-actions{display:flex;align-items:center;gap:5px;flex:none}.icon-button{width:28px;height:28px;display:inline-grid;place-items:center;border-radius:6px;color:var(--dim,var(--foreground-muted))}.hint{font-size:12px;color:var(--dim,var(--foreground-muted));line-height:1.7;background:none;padding:0}.bottom-note{margin-top:15px}.empty{padding:25px 0;font-size:13px}.field{display:flex;flex-direction:column;gap:8px;margin:0 0 23px;font-size:13px}.field>span,legend{font-weight:var(--medium,var(--font-medium))}.field>input,.field>textarea{width:100%;padding:10px 12px;border:0;border-radius:8px;background:var(--code,var(--code-bg));box-shadow:inset 0 0 0 var(--hairline,1px) var(--edge,var(--border));transition:box-shadow 140ms}.field>textarea{resize:vertical;min-height:80px;line-height:1.7}.field>input:focus,.field>textarea:focus{outline:0;box-shadow:inset 0 0 0 1px var(--accent,var(--primary)),0 0 0 3px color-mix(in srgb,var(--accent,var(--primary)) 10%,transparent)}.field small{color:var(--dim,var(--foreground-muted));font-size:12px;font-weight:var(--weight,400);line-height:1.7}fieldset{border:0;padding:0;margin:0 0 23px;min-width:0}legend{padding:0;margin-bottom:10px;font-size:13px}legend small{font-weight:var(--weight,400);color:var(--dim,var(--foreground-muted));font-size:12px;margin-left:8px}.options{display:flex;align-items:center;gap:12px 24px;flex-wrap:wrap}.options label{display:flex;align-items:center;gap:7px;white-space:nowrap;font-size:13px;cursor:pointer;min-height:29px}.options input{accent-color:var(--accent,var(--primary));width:15px;height:15px;margin:0;flex:none}.options+.hint{margin-top:9px}.protocols{column-gap:22px}.protocol-sample{padding-top:20px;box-shadow:inset 0 var(--hairline,1px) var(--line,var(--border-subtle));margin-bottom:24px}.protocol-sample fieldset{margin-bottom:6px}.hint code{background:none;font-size:12px;color:var(--muted,var(--foreground-muted));margin-left:5px}footer{display:flex;align-items:center;gap:9px;padding-top:20px;margin-top:28px;box-shadow:inset 0 var(--hairline,1px) var(--line,var(--border-subtle))}footer>.hint{margin-right:auto}.error{font-size:12px;color:var(--error)}.restored{display:flex;align-items:center;gap:6px;margin-bottom:20px}.undo-toast{position:sticky;bottom:20px;margin:0 auto 20px;display:flex;align-items:center;gap:14px;width:max-content;max-width:calc(100% - 30px);padding:9px 11px 9px 16px;border-radius:9px;background:var(--card,var(--surface-1));box-shadow:inset 0 0 0 var(--hairline,1px) var(--edge,var(--border));font-size:12px}.undo-toast>span{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.undo-toast>button:not(.icon-button){color:var(--accent,var(--primary));font-weight:var(--medium,var(--font-medium));white-space:nowrap}button:focus-visible{outline:0;box-shadow:inset 0 0 0 1.5px var(--accent,var(--primary))}@media(max-width:600px){.content{padding:22px 18px 40px}header{align-items:flex-start;gap:12px}h1{font-size:20px}.entries{padding:0 12px}.entry{gap:10px}.entry-icon{display:none}footer{flex-wrap:wrap}footer>.hint{width:100%;margin-bottom:6px}.options{gap:8px 18px}}
</style>
