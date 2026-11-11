import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pageTools, waitFor } from './electron-harness.mjs';

// 同一官方模型、真实 HTTP 收流及原生统计入口；不估算缺失的 token 或强制刷新 UI。
export async function verifyWorkbuddyStats({ request, page, session, output, check }) {
  const ui = pageTools(page), query = `?sessionId=${encodeURIComponent(session)}`;
  const save = (name, value) => writeFile(join(output, name + '.json'), JSON.stringify(value, null, 2));
  const before = await request('/api/settings/stats' + query);
  await ui.click('[data-testid=session-stats-trigger]');
  await waitFor(() => ui.exists('.session-stats-popover .metric-grid'), '原生会话统计');
  await page.evaluate(`(() => {
    window.__statsReceipts=[]; window.__statsUsageEvents=[];
    window.addEventListener('magi:usageChanged', event => window.__statsUsageEvents.push({at:Date.now(),detail:event.detail}));
    const original=window.fetch;
    window.fetch=async(...args)=>{const response=await original(...args);
      if(String(args[0]?.url??args[0]).includes('/api/settings/stats?')) {
        const value=await response.clone().json();window.__statsReceipts.push({at:Date.now(),value});
      }return response;};
  })()`);
  const start = await request('/api/session/turn', { scope:'personal',sessionId:session,
    text:'请直接写一篇约1200个汉字的短文，主题是观察海洋与潮汐，分六段连续输出。不要调用工具，不要代码块，不要提问。',
    requestId:'workbuddy-stats-long',userMessageId:'workbuddy-stats-long-user' });
  const finished = await waitFor(async () => (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(turn=>turn.turnId===start.turnId && ['completed','failed','cancelled','blocked'].includes(turn.status)), '真实 WorkBuddy 较长回复',180_000);
  await save('stats-long-turn', finished); assert.equal(finished.status,'completed');
  const after = await request('/api/settings/stats' + query);
  const attempts = finished.modelSteps.flatMap(step=>step.attempts??[]).filter(attempt=>attempt.transport?.outcome==='succeeded');
  await save('stats-counts', {before,after,attempts});
  const observed = await waitFor(()=>page.evaluate(`window.__statsReceipts.find(receipt=>receipt.value.version>=${after.version})`), '用量事件自动刷新会话统计',15_000);
  const events = await page.evaluate('window.__statsUsageEvents');
  await save('stats-refresh', {events,observed});
  check('模型结算后用量事件自动刷新原生统计，无需重新打开面板', events.some(event=>event.detail?.sessionId===session)
    && observed.value.totals.llmCallCount===after.totals.llmCallCount && after.totals.llmCallCount>before.totals.llmCallCount);
  const timing = attempts.map(attempt=>attempt.transport?.responseTiming).filter(Boolean);
  check('较长回复实际分段收流，正文首段时间和生成区间进入同一账本',timing.some(value=>value.firstContentMs>0&&value.contentDurationMs>0)
    && after.totals.performance.ttftSampleCount>before.totals.performance.ttftSampleCount);
  check('输入计数包含缓存，缓存读取不超过本次完整输入',attempts.every(attempt=>{
    const usage=attempt.transport?.usage;return !usage?.cacheReadIncludedInInput || (usage.cacheReadTokens??0)<=usage.inputTokens;
  }));
  const input = attempts.reduce((sum,attempt)=>{const usage=attempt.transport.usage;return sum+usage.inputTokens-(usage.cacheReadIncludedInInput?(usage.cacheReadTokens??0):0);},0);
  const cached=attempts.filter(attempt=>attempt.transport.usage?.cacheReadTokens!=null);
  check('新增输入与缓存样本按真实回执完整累计，不重复减去缓存',after.totals.netInputTokens-before.totals.netInputTokens===input
    && after.totals.cache.observedCallCount-before.totals.cache.observedCallCount===cached.length
    && after.totals.cache.readTokens-before.totals.cache.readTokens===cached.reduce((sum,attempt)=>sum+attempt.transport.usage.cacheReadTokens,0));
  const texts = await page.evaluate('({trigger:document.querySelector("[data-testid=session-stats-trigger]").innerText,panel:document.querySelector(".session-stats-popover").innerText})');
  await save('stats-ui',texts);
  check('原生面板自动显示最新累计输出和首 token 样本', texts.panel.includes(Number(after.totals.netOutputTokens).toLocaleString('zh-CN'))
    || texts.panel.includes((after.totals.netOutputTokens/1000).toLocaleString('zh-CN',{minimumFractionDigits:1,maximumFractionDigits:1})+'K'));
  const perf=after.totals.performance;
  const measured=attempts.filter(attempt=>attempt.transport?.usageObserved!==false && attempt.transport?.responseTiming?.responseDurationMs>0);
  const measuredOutput=measured.reduce((sum,attempt)=>sum+attempt.transport.usage.outputTokens,0);
  const measuredDuration=measured.reduce((sum,attempt)=>sum+attempt.transport.responseTiming.responseDurationMs,0);
  check('整体输出速度由真实总输出和完整请求计时累计，无需上游拆分思考 Token',measured.length>0
    && perf.responseSampleCount-before.totals.performance.responseSampleCount===measured.length
    && perf.responseOutputTokens-before.totals.performance.responseOutputTokens===measuredOutput
    && perf.responseDurationMs-before.totals.performance.responseDurationMs===measuredDuration);
  const speed=(perf.responseOutputTokens*1000/perf.responseDurationMs).toLocaleString('zh-CN',{minimumFractionDigits:1,maximumFractionDigits:1});
  check('面板与输入区显示同一真实整体速度，并说明包含思考与等待',texts.trigger.includes(speed+' tok/s')
    && texts.panel.replace(/\s/g,'').includes(speed+'tok/s') && texts.panel.includes('含思考与首段等待'));
  if(timing.every(value=>value.textOutputTokens==null)) {
    check('缺少正文拆分不妨碍整体速度，也不伪造正文速度',perf.tpsSampleCount===before.totals.performance.tpsSampleCount
      && after.totals.netOutputTokens>before.totals.netOutputTokens && perf.responseSampleCount>0);
  } else {
    check('明确的正文 token 拆分和计时可配对后才累计输出速度',perf.tpsSampleCount>before.totals.performance.tpsSampleCount);
  }
  await writeFile(join(output,'stats-updated.png'),Buffer.from(await ui.screenshot(),'base64'));
  await ui.click('.session-stats-popover .disclosure:last-of-type');
  await waitFor(()=>page.evaluate('document.querySelector(".session-stats-popover")?.innerText.includes("正文平均输出速度")'),'独立正文速度明细');
  await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
  try {
    await page.evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
    const layout=await page.evaluate(`(()=>{const panel=document.querySelector('.session-stats-popover'),r=panel.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:innerWidth,height:innerHeight,overflow:[...panel.querySelectorAll('.metric,.detail-body')].some(node=>node.scrollWidth>node.clientWidth+1)};})()`);
    await save('stats-narrow-layout',layout);
    check('390px 统计面板和性能明细保持可见，无横向裁切',layout.left>=0&&layout.right<=layout.width+1&&layout.top>=0&&layout.bottom<=layout.height+1&&!layout.overflow);
    await writeFile(join(output,'stats-narrow.png'),Buffer.from(await ui.screenshot(),'base64'));
  } finally {await page.call('Emulation.clearDeviceMetricsOverride');}
  await ui.click('[data-testid=session-stats-trigger]');
  return {sessionId:session,stats:after};
}
