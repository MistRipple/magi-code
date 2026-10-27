import type { ContentBlock, Message, TimelineRenderItem, ToolCallStatus } from '../types/message';
const message = (id: string, type: Message['type'], content: string, blocks: ContentBlock[] = [], metadata: Message['metadata'] = {}): TimelineRenderItem => ({
  key:id, message:{id,type,role:'assistant',source:'orchestrator',content,blocks,timestamp:1700000000000,isStreaming:false,isComplete:true,metadata:{turnId:'card-acceptance',...metadata}},
});
export const call = (id: string,name: string,status: ToolCallStatus,args: Record<string,unknown>,result?: unknown): ContentBlock => ({
  id, type:'tool_call',content:'',toolCall:{id,name,status,arguments:args,result:typeof result === 'string' ? result : result === undefined ? undefined : JSON.stringify(result),durationMs:1234},
});
const spacingUser = message('spacing-user', 'user_input', '允许 Magi 配置连接器');
spacingUser.message.role = 'user';
spacingUser.message.source = 'user';
export const messageSpacingItems: TimelineRenderItem[] = [
  spacingUser,
  message('spacing-answer', 'text', '连接器配置结果尚未确认。请在右侧完成后继续，或点击「检查并重试连接器」重新授权。Magi 会先检查，缺少时创建，已有连接器需要刷新时重试刷新。', [], {assistantOutputKind:'final',conversationPresentationRole:'final'}),
];
export const acceptanceItems: TimelineRenderItem[] = [
  message('thinking','thinking','',[{id:'thinking-block',type:'thinking',content:'',thinking:{groupId:'thinking-group',status:'completed',isStreaming:false,segments:[{segmentId:'segment',messageId:'thinking',status:'completed',content:'先沿现有协议读取事实，再修改展示层。保留审批、产物与用户手动展开状态。'}]}}]),
  message('progress','text','我会检查工具调用与终端输出，再核对文件变更和不同状态。'),
  message('search','tool_call','',[call('search-call','search_text','success',{query:'tool-header',path:'web/src'},'ToolCall.svelte: header\nTerminalSessionCard.svelte: header')]),
  message('file-read','tool_call','',[call('file-read-call','file_read','success',{path:'src/很长的组件文件名用于验收后缀保留与窄屏溢出.svelte'},'const aligned = true;')]),
  message('skill','tool_call','',[call('skill-call','skill_apply','success',{skill:'ui-design'},'已加载组件规范。')]),
  message('http','tool_call','',[call('mcp-call','mcp__knowledge__search','running',{query:'产品化设计规范'})]),
  message('list','tool_call','',[call('list-call','file_read','success',{path:'src/',type:'directory'},{entries:[{name:'ToolCall.svelte',type:'file'},{name:'styles',type:'directory'}]})]),
  message('shell','tool_call','',[call('shell-call','shell_exec','success',{command:'npm run check'},{terminal_id:1,status:'succeeded',command:'npm run check',cwd:'/workspace/magi',output:'svelte-check found 0 errors and 0 warnings\nTests passed',exit_code:0})]),
  message('error','tool_call','',[call('error-call','web_fetch','error',{url:'https://example.invalid'},{status:'failed',error_code:'network_timeout',error:'连接超时，请检查网络后重试。'})]),
  message('unconfirmed','tool_call','',[call('unconfirmed-call','browser_click','unconfirmed',{element_ref:'e:1'},{status:'indeterminate',error_code:'desktop_disconnected',error:'操作已发出，但无法确认是否生效，请先核对页面。'})]),
  message('cancelled','tool_call','',[call('cancelled-call','web_search','cancelled',{query:'已取消的检索'},'用户停止了本次调用。')]),
  message('artifact','tool_call','',[{id:'file',type:'file_change',content:'',fileChange:{filePath:'src/很长的组件文件名用于验收后缀保留与窄屏溢出.svelte',changeType:'modify',additions:3,deletions:1,diff:'@@ -1,2 +1,4 @@\n-const size = 20;\n+const size = 14;\n+const gap = 8;\n+const aligned = true;\n export { size };'}}],{conversationPresentationRole:'artifact'}),
  message('agent','tool_call','',[call('agent-call','agent_spawn','success',{role:'reviewer',display_name:'检查工具卡片的一致性'},{status:'completed',child_task_id:'acceptance-agent',title:'检查工具卡片的一致性',role:'reviewer'})],{conversationPresentationRole:'delegation',agentChildStatus:'completed'}),
  message('approval','tool_call','',[call('approval-call','shell_exec','pending',{command:'npm run build'},{status:'awaiting_approval',approval:{approvalId:'acceptance-approval',sessionId:'acceptance-session',taskId:'acceptance-task',turnId:'card-acceptance',toolCallId:'approval-call',toolName:'shell_exec',reason:'构建会在当前工作区生成本地文件。'}})],{conversationPresentationRole:'attention'}),
  message('plan','text','',[{id:'plan-block',type:'plan',content:'',plan:{goal:'统一工具卡片标题、状态和产物层级。',constraints:['沿用主题变量','不修改业务事实'],acceptanceCriteria:['详细与摘要模式均可阅读与操作']}}]),
  message('file-empty','tool_call','',[{id:'file-empty-block',type:'file_change',content:'',fileChange:{filePath:'docs/no-change.md',changeType:'modify',additions:0,deletions:0,diff:''}}]),
  message('file-binary','tool_call','',[{id:'file-binary-block',type:'file_change',content:'',fileChange:{filePath:'assets/report.pdf',changeType:'create',contentKind:'binary',size:2048,mime:'application/pdf',additions:0,deletions:0,diff:''}}]),
  message('file-unavailable','tool_call','',[{id:'file-unavailable-block',type:'file_change',content:'',fileChange:{filePath:'docs/preview-unavailable.md',changeType:'modify',error:'preview unavailable',additions:3,deletions:1,diff:''}}]),
  message('steps','text','',[],{executionSteps:{executionId:'acceptance-execution',turnId:'card-acceptance',taskId:'acceptance-task',revision:1,createdAt:1700000000000,updatedAt:1700000000000,steps:[{stepId:'read',title:'核对工具卡片与主题来源',status:'completed',dependsOn:[],evidenceRefs:[],taskIds:[]},{stepId:'verify',title:'验证窄屏、状态和折叠交互',status:'running',dependsOn:['read'],evidenceRefs:[],taskIds:[]}]}}),
  message('question','tool_call','',[call('question-call','ask_user_question','success',{questions:[{header:'输出方式',question:'交付内容如何展示？',multiSelect:false,options:[{label:'正文与代码',description:'保留可阅读的结果'},{label:'文件链接',description:'在右侧预览文件'}]}]},{status:'succeeded',answers:[{header:'输出方式',question:'交付内容如何展示？',selected:['正文与代码'],other:null}]})]),
  message('image-pending','tool_call','',[call('image-call','image_generate','running',{prompt:'产品卡片示意图',size:'1536x1024'})],{conversationPresentationRole:'artifact'}),
  message('image-ready','tool_call','',[call('image-ready-call','image_generate','success',{prompt:'低对比卡片规范'},{status:'succeeded',path:'card-preview.svg',media_type:'image/svg+xml',bytes:600,revised_prompt:'卡片结构示意，背景与前景由当前皮肤控制。'})],{conversationPresentationRole:'artifact'}),
  message('model-failure','error','',[],{assistantOutputKind:'error',modelFailure:{schemaVersion:'model-failure.v1',code:'response_stream_interrupted',summary:'响应流中断，请检查连接后继续。',detail:'连接在最后一段输出前关闭；已收到的内容仍然保留。',stage:'response_stream',retryable:true,retryAttempts:1}}),
  message('answer','text','正文、工具卡片和底部耗时共用左侧基准。\n\n158. 先核对消息正文的边界。\n159. 保留列表自然缩进，编号不能越过内容边界。\n160. 最后检查代码、工具和耗时。\n\n已统一工具卡片：箭头最右、状态圆点次之，耗时和其他信息依次向左。\n\n```javascript\nconst greeting = "你好";\nconsole.log(greeting);\n// 祝你每天都有好心情\n```\n\n```typescript\nconst columns = [metadata, status, disclosure];\n```\n\n```json\n{ \"layout\": \"trailing\", \"endpoint\": \"https://api.example.com/v1/teams/design/message-card-previews\" }\n```\n\n```text\n轻量文本输出，不再堆叠完整工具标题栏。\n```\n\n```mermaid\nflowchart LR\n  Input --> Tool --> Result\n```',[],{assistantOutputKind:'final',conversationPresentationRole:'final',responseDurationMs:306000}),
];
