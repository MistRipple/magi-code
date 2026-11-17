from pathlib import Path
root=Path(__file__).resolve().parent
s=(root/'dir-3.html').read_text()
s=s.replace('<title>Magi 工具设置 — 紧凑目录</title>','<title>Magi 工具设置 — 方案 C 细化</title>')
s=s.replace('<body>','''<body>
<!--
方案 C 已获用户明确选择。服务展开后直接展示工具名称与用途说明，不展示传参。
范围：设置/工具主页交互原型；所有服务、数量及操作回执均为示例。
复用 Magi 主题、字体、按钮和 880px 设置内容列，不修改正式产品。
本原型预览的密度与场景控制，不属于待新增的产品设置。
-->''',1)
start=s.index('<div class="demo-bar">');end=s.index('<div class="app">',start)
s=s[:start]+'''<div class="demo-bar"><strong>方案 C / 紧凑目录</strong><span class="demo-note">交互原型，所有配置与回执均为示例</span><nav class="demo-links" aria-label="原型预览"><a class="demo-link" href="index.html">初稿对比</a><label class="preview-control">密度<select id="density" aria-label="原型密度"><option value="compact">紧凑</option><option value="comfortable">舒适</option></select></label><label class="preview-control">场景<select id="scenario" aria-label="演示状态"><option value="normal">正常目录</option><option value="empty">空目录</option><option value="connection-error">连接失败</option><option value="refresh-error">刷新时失败</option></select></label></nav><button class="theme" data-action="theme" aria-label="切换浅深主题"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2"/></svg><span>切换主题</span></button></div>
'''+s[end:]
s=s.replace('<p class="sub" style="margin-bottom:16px">按服务浏览工具目录，展开查看说明与参数。</p>','')
start=s.index('<div class="footnote"><label for="scenario">');end=s.index('</div></div></main>',start)
s=s[:start]+''+s[end+6:]
# Search temporarily discloses matches without mutating the user's manual choices.
s=s.replace("function row(s){return `", "function row(s){const expanded=open.has(s.id)||Boolean(query);return `")
s=s.replace("open.has(s.id)?", "expanded?")
s=s.replace('${open.has(s.id)}','${expanded}')
# Only row() used those expressions. Keep the explicit expand action unchanged.
s=s.replace('${s.state===\'error\'?\'—\':s.tools.length+\' 个\'}', '${s.state===\'error\'?\'未取得\':s.tools.length+\' 个\'}')
s=s.replace('<span class="table-count">','<span class="table-count" title="上次成功读取的目录数量，连接状态单独展示">')
s=s.replace("${busy.has(s.id)?'disabled':''}","${busy.has(s.id)||!s.enabled?'disabled':''} aria-busy=\"${busy.has(s.id)}\"")
s=s.replace("s.id+'/'+t.name,isOpen", "s.id+'/'+t.name,isOpen")
s=s.replace("if(action==='toggle'){s.enabled", "if(action==='toggle'&&busy.has(id))return;\nif(action==='toggle'){s.enabled")
s=s.replace("if(action==='refresh'||action==='retry'){if(busy.has(id))return;", "if(action==='refresh'||action==='retry'){if(busy.has(id)||!s.enabled)return;")
s=s.replace("setTimeout(()=>{busy.delete(id);if(action==='retry')", "setTimeout(()=>{if(!servers.some(current=>current===s))return;busy.delete(id);if(action==='retry')")
s=s.replace("s.tools=s.tools.length?s.tools:[{...toolsFile[0],name:'list_resources',desc:'列出可用设计资源。',full:'读取资源目录中的名称、类型与来源链接。'}]", "s.tools=s.tools.length?s.tools:[{name:'list_resources',desc:'列出可用设计资源。',full:'读取资源目录中的名称、类型与来源链接。',params:[['collection','string','可选','资源集合名称']]}]")
s=s.replace("servers=servers.filter(s=>s.id!==id);document", "servers=servers.filter(s=>s.id!==id);open.delete(id);busy.delete(id);delete errors[id];for(const key of expandedTools){if(key.startsWith(id+'/'))expandedTools.delete(key)}document")
s=s.replace("s.name=name;s.endpoint=endpoint;s.kind=kind", "s.name=name;if(s.endpoint!==endpoint||s.kind!==kind){s.tools=[];s.state='pending';delete errors[s.id];for(const key of expandedTools){if(key.startsWith(s.id+'/'))expandedTools.delete(key)}}s.endpoint=endpoint;s.kind=kind")
# A pending connection is not represented as success or as a known zero-tool directory.
s=s.replace("s.state==='error'?'连接失败':'已连接'", "s.state==='error'?'连接失败':s.state==='pending'?'待连接':'已连接'")
s=s.replace("s.state==='error'?'error':'ok'", "s.state==='error'?'error':s.state==='pending'?'off':'ok'")
s=s.replace("s.state==='error'?'未取得':s.tools.length+' 个'", "s.state==='error'||s.state==='pending'?'未取得':s.tools.length+' 个'")
s=s.replace("s.state==='error'?'未取得':s.tools.length+' 个工具'", "s.state==='error'||s.state==='pending'?'未取得':s.tools.length+' 个工具'")
s=s.replace("${s.enabled&&s.state==='error'?", "${s.enabled&&s.state==='pending'?`<div class=\"notice\">${icon('info')}<div><strong>连接配置已变更</strong><p>原工具目录已失效，请重新连接后读取。</p></div><button class=\"btn\" data-action=\"retry\" data-id=\"${s.id}\">连接并读取</button></div>`:s.enabled&&s.state==='error'?")
s=s.replace("icon:'plug',tools:[]", "icon:'plug',tools:[]")
s=s.replace("kind,enabled:true,state:'ok'", "kind,enabled:true,state:'pending'")
s=s.replace("data-action=\"edit\" data-id=\"${s.id}\"", "data-action=\"edit\" data-id=\"${s.id}\" ${busy.has(s.id)?'disabled':''}")
s=s.replace("delete errors[id];feedback='演示：目录已更新'", "delete errors[id];s.state='ok';feedback='演示：目录已更新'")
# Every rendered note reflects current visible/total counts; no static dashboard statistics.
s=s.replace("document.getElementById('server-count').textContent=servers.length+' 个服务'", "document.getElementById('server-count').textContent=(query||filter!=='all')?list.length+' / '+servers.length+' 个服务':servers.length+' 个服务'")
s=s.replace("data-id=\"${s.id}\"></button><button", "data-id=\"${s.id}\" ${busy.has(s.id)?'disabled':''}></button><button",1)
# Tool rows describe capabilities; invocation details are outside the user's task.
tool_start=s.index('function toolHtml(s,t){')
tool_end=s.index('function directory(s){',tool_start)
s=s[:tool_start]+r'''function toolHtml(s,t){
  const description=t.full||t.desc||'上游未提供工具说明。';
  const long=description.length>100;
  return `<div class="tool tool-info-row"><code class="tool-info-name">${esc(t.name)}</code><div class="tool-info-copy">${long?`<details class="tool-description-details"><summary><span class="tool-info-preview">${esc(description.slice(0,90))}…</span><span class="description-toggle"><span class="show-more">展开说明</span><span class="show-less">收起说明</span></span></summary><p class="tool-info-description">${esc(description)}</p></details>`:`<p class="tool-info-description">${esc(description)}</p>`}</div></div>`;
}
'''+s[tool_end:]
s=s.replace("let open=new Set(['files']),selected='files',expandedTools=new Set()", "let open=new Set(['files']),selected='files'")
s=s.replace("if(action==='tool'){expandedTools.has(id)?expandedTools.delete(id):expandedTools.add(id)}", '')
s=s.replace("for(const key of expandedTools){if(key.startsWith(id+'/'))expandedTools.delete(key)}", '')
s=s.replace("for(const key of expandedTools){if(key.startsWith(s.id+'/'))expandedTools.delete(key)}", '')
s=s.replace("(t.name+' '+t.desc)", "(t.name+' '+(t.full||t.desc))")
s=s.replace("full:'按关键词查找文档，返回标题、摘要和来源链接。'", "full:'按关键词查找已接入的文档内容，返回匹配的标题、摘要与来源链接。适合查找团队说明、开发文档和操作指引。搜索结果可以帮助定位相关章节；需要阅读完整内容时，再使用文档读取工具。结果范围取决于该服务实际接入的文档及其访问权限，未接入或无权访问的内容不会出现在搜索结果中。'")
# The previous interactive parameter cards are removed rather than merely hidden.
import re
s=re.sub(r'\.params[^{}]*\{[^{}]*\}', '',s)
# Counts belong to the service row; refresh is a service action, not a second list header.
header_start=s.index('<div class="directory-head">',s.index('function directory(s){'))
header_end=s.index('</button></div>',header_start)+len('</button></div>')
s=s[:header_start]+s[header_end:]
s=s.replace('return `<div class="actions">', 'return `<div class="actions"><button type="button" class="icon-btn" data-action="refresh" data-id="${s.id}" aria-label="刷新${esc(s.name)}工具目录" title="刷新工具目录" ${busy.has(s.id)||!s.enabled?\'disabled\':\'\'} aria-busy="${busy.has(s.id)}">${icon(\'refresh-cw\',busy.has(s.id)?\'spin\':\'\')}</button>',1)
# Preserve product controls; expose only sensible preview density options.
extra_css='''
.preview-control{display:flex;align-items:center;gap:6px;font-size:12px;color:var(--muted)}.preview-control select{min-height:32px;background:transparent;padding:4px 8px}.demo-links{align-items:center;gap:12px}.table-head,.table .service-row{grid-template-columns:minmax(150px,1fr) 100px 80px 160px}.table .service-row .table-count{white-space:nowrap}.table .service-detail{padding-inline-start:40px;padding-block-start:2px}.params th:nth-child(1){width:30%}.params th:nth-child(2){width:20%}.params th:nth-child(3){width:50%}.tool-body{padding:0 12px 16px 0}.tool-summary .identity{min-width:0}.tool-summary .chev{transform:none}.service-primary .chev{transform:none}.service.open .service-primary .chev{transform:rotate(90deg)}.service-primary .sub{margin-top:1px}.notice summary{min-height:32px}.table .tool-summary .table-tool-desc{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.table .tool-body p{max-width:100%;overflow-wrap:anywhere}.tool-title{font-size:12px;font-weight:600}.service-detail:empty{display:none}.table .service-row:has(button:focus-visible){background:var(--hover)}
[data-density=comfortable] .table .service-row{min-height:68px;padding-block:12px}[data-density=comfortable] .table .tool-summary{min-height:64px}[data-density=comfortable] .params td{padding-block:12px}
@media(max-width:1000px) and (min-width:701px){.table-head,.table .service-row{grid-template-columns:minmax(110px,1fr) 88px 62px 160px}}
@media(max-width:700px){.demo-links{gap:6px;justify-content:start;flex-wrap:wrap}.demo-link{padding-inline:4px}.preview-control{gap:4px}.preview-control select{min-height:44px;max-width:115px}.table .service-row{display:flex}.table .service-row>.state{margin-inline-start:0}.table .service-detail{padding-inline-start:12px}.table .service-primary{min-height:44px}.table .tool-summary{min-height:56px}.notice summary{min-height:44px}.tool-body{padding-inline-end:0}.table .tool-description{white-space:normal}.notice pre{max-width:100%}.footnote{padding-top:16px}}
'''
s=s.replace('</style>',extra_css+'\n.tool-info-row{display:grid;grid-template-columns:minmax(120px,200px) minmax(0,1fr);gap:20px;align-items:baseline;padding:11px 0;min-height:44px}.tool-info-name{font-size:12px;font-weight:500;overflow-wrap:anywhere;white-space:normal;color:var(--fg)}.tool-info-copy{min-width:0}.tool-info-description,.tool-info-preview{margin:0;font-size:12px;line-height:1.65;color:var(--muted);overflow-wrap:anywhere}.tool-info-row:hover{background:none}.tool-description-details{min-width:0}.tool-description-details summary{list-style:none;display:flex;flex-direction:column;align-items:start;gap:2px;cursor:pointer;min-height:32px}.tool-description-details summary::-webkit-details-marker{display:none}.description-toggle{color:var(--muted);font-size:12px;text-decoration:underline;text-underline-offset:3px;min-height:24px;display:inline-flex;align-items:center}.show-less{display:none}.tool-description-details[open] .show-more,.tool-description-details[open] .tool-info-preview{display:none}.tool-description-details[open] .show-less{display:inline}.tool-description-details[open] .tool-info-description{margin-top:4px}.tool-description-details summary:focus-visible{outline:2px solid var(--accent);outline-offset:3px;border-radius:4px}[data-density=comfortable] .tool-info-row{padding-block:15px;min-height:56px}@media(max-width:900px){.tool-info-row{grid-template-columns:minmax(110px,150px) minmax(0,1fr);gap:14px}}@media(max-width:700px){.tool-info-row{grid-template-columns:minmax(0,1fr);gap:3px;padding-block:12px}.tool-info-name{font-size:12px}.tool-info-description,.tool-info-preview{font-size:12px}.description-toggle{min-height:44px}.tool-description-details summary{min-height:44px}}\n'+'</style>')
s=s.replace("renderNav();renderCatalog();renderSecondary();", "document.documentElement.dataset.density=localStorage.getItem('magi-tools-demo-density')||'compact';document.getElementById('density').value=document.documentElement.dataset.density;document.getElementById('density').addEventListener('change',event=>{document.documentElement.dataset.density=event.target.value;localStorage.setItem('magi-tools-demo-density',event.target.value)});renderNav();renderCatalog();renderSecondary();")
(root/'tools-directory.html').write_text(s)
print('Refined selected C prototype.')
