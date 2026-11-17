import {chromium} from '/Users/xie/.npm/_npx/e41f203b7505f1fb/node_modules/playwright/index.mjs';
import {resolve} from 'node:path';import {writeFile} from 'node:fs/promises';
const root=resolve('design-demos/tools-settings');
const browser=await chromium.launch({executablePath:'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
const results=[];
for(let i=1;i<=3;i++){
 const p=await browser.newPage({viewport:{width:1440,height:1000},deviceScaleFactor:1});const errors=[];p.on('pageerror',e=>errors.push(e.message));
 await p.goto('file://'+root+`/dir-${i}.html`);await p.waitForTimeout(250);await p.screenshot({path:root+`/dir-${i}.png`,fullPage:true});
 await p.locator('[data-action=tool][data-id="files/read_file"]').click();await p.waitForTimeout(200);if(!await p.getByText('start_line',{exact:true}).isVisible())throw new Error('Parameters not visible');await p.screenshot({path:root+`/dir-${i}-detail.png`});
 await p.locator('#q').fill('search_files');if(await p.locator('.service').count()>1)throw new Error('Search failed');await p.locator('#q').fill('');
 if(i===2)await p.locator('[data-action=select][data-id=design]').click();else await p.locator('[data-action=expand][data-id=design]').first().click();
 if(!await p.getByText('无法读取工具目录',{exact:true}).isVisible())throw new Error('Missing inline error');
 await p.locator('[data-action=retry][data-id=design]').click();await p.waitForTimeout(950);if(!await p.locator('#feedback').textContent())throw new Error('Missing recovery');
 await p.locator('#scenario').selectOption('refresh-error');await p.locator('[data-action=refresh][data-id=files]').click();await p.waitForTimeout(950);if(!await p.getByText('刷新失败',{exact:true}).isVisible())throw new Error('Refresh failure');if(!await p.getByText('read_file',{exact:true}).isVisible())throw new Error('Cache lost');
 await p.locator('[data-action=retry][data-id=files]').click();await p.waitForTimeout(950);
 await p.locator('[data-action=toggle][data-id=files]').click();if(await p.locator('[data-action=toggle][data-id=files]').getAttribute('aria-checked')!=='false')throw new Error('Toggle failed');
 await p.locator('[data-action=add]').click();await p.locator('#service-name').fill('演示新增服务');await p.locator('#service-address').fill('https://example.com/mcp');await p.locator('#editor-form button[type=submit]').click();
 if(!await p.locator('#server-count').textContent().then(t=>t.includes('5')))throw new Error('Add failed');
 await p.locator('[data-action=theme]').click();await p.waitForTimeout(200);await p.screenshot({path:root+`/dir-${i}-dark.png`,fullPage:true});
 await p.evaluate(()=>localStorage.removeItem('magi-tools-demo-theme'));await p.reload();
 await p.setViewportSize({width:375,height:812});await p.screenshot({path:root+`/dir-${i}-mobile.png`,fullPage:false});
 const overflow=await p.evaluate(()=>document.documentElement.scrollWidth>innerWidth);if(overflow)throw new Error(`Mobile overflow ${i}`);
 results.push({direction:i,consoleErrors:errors,mobileOverflow:overflow,checks:['search','service-selection-or-disclosure','tool-parameters','connection-failure','retry','refresh-failure-retains-directory','enable-disable','add-form','light-dark','375px-layout']});
 await p.close();
}
await writeFile(root+'/verification.json',JSON.stringify(results,null,2));await browser.close();console.log(JSON.stringify(results));
