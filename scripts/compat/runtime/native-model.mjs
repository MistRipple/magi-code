// 原页面模型业务消费 daemon 的同一模型选择、流式调用及账本，不创建会话执行器。
import { AsyncLocalStorage } from 'node:async_hooks';
import { randomUUID } from 'node:crypto';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
import { dshReasoningOf } from './reasoning.mjs';

export const NATIVE_MODEL_PROVIDER = 'magi-native';
const MAX_INPUT = 3 * 1024 * 1024;

async function messagesOf(ctx, options, selection, signal) {
  const messages = [];
  if (options.system !== undefined) {
    if (typeof options.system !== 'string') throw new Error('system 必须为原始文本');
    if (options.system) messages.push({ role:'system', content:options.system });
  }
  if (!Array.isArray(options.messages)) throw new Error('模型 messages 必须为数组');
  let imageBytes = 0;
  for (const message of options.messages) {
    const replay = message.source?.replayState?.response;
    if (!['system','developer','user','assistant','tool'].includes(message?.role) || !Array.isArray(message.content)
      || Object.keys(message).some(key=>!['id','role','content','source','timestamp','toolCallId','isError'].includes(key))) throw new Error('模型消息格式无效');
    if (replay && (message.role!=='assistant' || replay.version!==1 || replay.revision!==selection.revision || !Array.isArray(replay.providerContext))) throw new Error('模型续写数据不属于当前模型快照');
    let content = ''; const images = [], toolCalls = [];
    for (const block of message.content) {
      signal.throwIfAborted();
      if (block.type==='text' && typeof block.text==='string') content += block.text;
      else if (block.type==='reasoning' && replay) continue; // 原生 providerContext 拥有无损推理回放。
      else if (block.type==='tool-call' && message.role==='assistant' && typeof block.id==='string' && typeof block.name==='string' && typeof block.arguments==='string') {
        toolCalls.push({id:block.id,type:'function',function:{name:block.name,arguments:block.arguments}});
      } else if (block.type==='image' && ['user','tool'].includes(message.role)) {
        const image = await ctx.attachments.readImageRequest(block.attachment, {width:2048,height:2048,maxBytes:512*1024}, signal);
        imageBytes += image.data.length;
        if (image.data.length>512*1024 || imageBytes>2*1024*1024) throw new Error('本次模型图片超过 2 MiB 请求预算，请缩小图片后重试');
        images.push({type:'base64',media_type:image.mediaType,data:image.data.toString('base64')});
      } else throw new Error(`模型消息包含无效内容：${block.type}`);
    }
    if (message.role==='tool' && typeof message.toolCallId!=='string') throw new Error('工具结果缺少 toolCallId');
    if (message.role==='tool' && (message.isError || images.length)) {
      content = JSON.stringify({model_content:[{type:'text',text:content},...images.map(source=>({type:'image',source}))],is_error:message.isError===true});
      images.length=0;
    }
    messages.push({role:message.role,content,
      ...(images.length ? {images}: {}), ...(toolCalls.length ? {toolCalls}: {}),
      ...(message.role==='tool' ? {toolCallId:message.toolCallId}: {}),
      ...(replay ? {providerContext:replay.providerContext}: {}),
    });
  }
  if (Buffer.byteLength(JSON.stringify(messages))>MAX_INPUT) throw new Error('模型消息超过 3 MiB 请求上限');
  return messages;
}

// 有界的本次传输队列；消费者退出即取消同一模型操作，不保存第二份业务结果。
async function* modelResult(api, input, signal) {
  const controller = new AbortController(), combined = AbortSignal.any([signal,controller.signal]);
  const queue = []; let bytes=0, wake, settled=false, result, failure;
  const pending = api.model.invoke(input,{signal:combined,onDelta(delta) {
    const size=Buffer.byteLength(JSON.stringify(delta));
    if (queue.length>=256 || bytes+size>1024*1024) throw new Error('模型流消费者过慢，请取消后重试');
    bytes+=size; queue.push({delta,size}); wake?.();
  }}).then(value=>{result=value;},error=>{failure=error;}).finally(()=>{settled=true;wake?.();});
  try {
    while (!settled || queue.length) {
      if (!queue.length && !settled) await new Promise(resolve=>{wake=resolve;});
      wake=undefined;
      while(queue.length) { const item=queue.shift();bytes-=item.size;combined.throwIfAborted();yield {delta:item.delta}; }
    }
    combined.throwIfAborted();
    if (failure) throw failure;
    yield {result};
  } finally { controller.abort(); await pending; }
}

export function installNativeModel(ctx, api) {
  const operations = new AsyncLocalStorage();
  const current = () => {
    const scope=operations.getStore();
    if (!scope || scope.closed) throw new Error('模型选择需要活动的插件页面业务');
    scope.signal.throwIfAborted();
    if (scope.selectionError) throw scope.selectionError;
    return scope;
  };
  ctx.provide('agentDefaultModel',Object.freeze({currentSelection() {
    const {selection}=current();return Object.freeze({provider:NATIVE_MODEL_PROVIDER,model:selection.model});
  }}));
  ctx.llm.registerAdapter([NATIVE_MODEL_PROVIDER],new class extends LlmAdapter {
    providerInfo() { return {id:NATIVE_MODEL_PROVIDER,name:'Magi'}; }
    providerRetryPolicy() { return {mode:'normal',maxRetries:0,retryableCodes:[],initialDelayMs:0,maxDelayMs:0,jitterRatio:0}; }
    async listModels(provider) {
      const {selection}=current();
      return [await this.resolveModel(provider,selection.model)];
    }
    async resolveModel(provider,model) {
      const {selection}=current();
      if(provider!==NATIVE_MODEL_PROVIDER || model!==selection.model) throw new Error('模型选择已变化');
      return {provider,id:model,name:model,defaultMaxTokens:4096,
        ...(selection.inputModalities ? {inputModalities:selection.inputModalities}: {}),
        ...(selection.reasoning ? {reasoning:dshReasoningOf(selection.reasoning)}: {}),
      };
    }
    async *stream(options) {
      const scope=current();
      const allowed=new Set(['provider','model','system','messages','maxTokens','signal','tools','toolHistory','temperature','stop','reasoningEffort','sessionId','purpose']);
      for(const key of Object.keys(options)) if(!allowed.has(key) && options[key]!==undefined) throw new Error(`模型请求包含未知参数：${key}`);
      if(options.provider!==NATIVE_MODEL_PROVIDER || options.model!==scope.selection.model) throw new Error('请求与已选择模型不一致');
      const signal=options.signal ? AbortSignal.any([scope.signal,options.signal]):scope.signal;
      const messages=await messagesOf(ctx,options,scope.selection,signal);
      const input={operationId:randomUUID(),selection:scope.selection,messages,maxOutputTokens:options.maxTokens??4096,
        ...(options.tools?.length ? {tools:options.tools.map(tool=>{
          if(tool.deferLoading) throw new Error('原生模型使用完整工具定义，不接受 deferLoading');
          return {type:'function',origin:'plugin',function:{name:tool.name,description:tool.description,parameters:tool.parameters}};
        })}: {}),
        ...(options.temperature===undefined ? {}: {temperature:options.temperature}),
        ...(options.stop===undefined ? {}: {stop:options.stop}),
        ...(options.reasoningEffort===undefined ? {}: {reasoningEffort:options.reasoningEffort==='off'?'none':options.reasoningEffort}),
        ...(options.sessionId===undefined && options.purpose===undefined ? {}: {metadata:{sessionId:options.sessionId,purpose:options.purpose}}),
      };
      const blocks=new Map(); let nextIndex=0, result;
      function* append(type,text) {
        if(!text) return;
        let block=blocks.get(type);
        if(!block) {block={index:nextIndex++,text:''};blocks.set(type,block);yield {type:'block-start',index:block.index,blockType:type};}
        block.text+=text;yield {type:type==='text'?'text-delta':'reasoning-delta',index:block.index,text};
      }
      for await (const item of modelResult(api,input,signal)) {
        if(item.result) {result=item.result;continue;}
        if(item.delta.replace) throw new Error('模型在已交付后改写正文，不能将旧内容当作有效输出');
        yield* append('reasoning',item.delta.thinking);yield* append('text',item.delta.content);
      }
      const limited=['length','max_tokens','max_output_tokens'].includes(result.finishReason);
      if(!['completed','requires_tool_execution'].includes(result.status) && !(result.status==='incomplete'&&limited)) throw new Error(`模型业务未完成：${result.status}`);
      for(const [type,full] of [['reasoning',result.thinking??''],['text',result.content??'']]) {
        const prior=blocks.get(type)?.text??'';
        if(!full.startsWith(prior)) throw new Error('模型最终结果与已交付片段不一致');
        yield* append(type,full.slice(prior.length));
      }
      for(const [type,block] of blocks) yield {type:'block-end',index:block.index,block:{type,text:block.text}};
      for(const call of result.toolCalls??[]) {
        const index=nextIndex++, block={type:'tool-call',id:call.id,name:call.function.name,arguments:call.function.arguments};
        yield {type:'block-start',index,blockType:'tool-call'};
        yield {type:'tool-call-delta',index,id:block.id,name:block.name,argumentsDelta:block.arguments};
        yield {type:'block-end',index,block};
      }
      if(result.usage) {
        const u=result.usage,read=u.cacheReadTokens??0;
        if(Number.isSafeInteger(u.inputTokens)&&Number.isSafeInteger(u.outputTokens)) yield {type:'usage',usage:{
          inputTokens:u.inputTokens-(u.cacheReadIncludedInInput?read:0),outputTokens:u.outputTokens,
          ...(u.cacheReadTokens==null?{}:{cacheReadTokens:read}),...(u.cacheWriteTokens==null?{}:{cacheWriteTokens:u.cacheWriteTokens}),
          ...(u.reasoningTokens==null?{}:{reasoningTokens:u.reasoningTokens}),
        }};
      }
      yield {type:'finish',reason:{kind:limited?'max-tokens':result.status==='requires_tool_execution'?'tool-calls':'stop'},
        ...(result.providerContext?.length?{replayState:{response:{version:1,revision:scope.selection.revision,providerContext:result.providerContext}}}:{}),
      };
    }
  }());
  return {async run(callback,signal) {
    signal.throwIfAborted();const scope={signal,closed:false};
    try {scope.selection=await api.model.selection();} catch(error) {scope.selectionError=error;}
    signal.throwIfAborted();
    try {return await operations.run(scope,callback);} finally {scope.closed=true;}
  }};
}
