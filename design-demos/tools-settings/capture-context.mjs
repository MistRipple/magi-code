import {chromium} from '/Users/xie/.npm/_npx/e41f203b7505f1fb/node_modules/playwright/index.mjs';
const browser=await chromium.launch({executablePath:'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
const page=await browser.newPage({viewport:{width:1440,height:1000},deviceScaleFactor:1});
await page.goto('http://127.0.0.1:38123/web.html');
await page.waitForTimeout(1500);
console.log((await page.locator('button').evaluateAll(a=>a.map(x=>({text:x.textContent?.trim().slice(0,40),title:x.title,label:x.getAttribute('aria-label'),'data-testid':x.getAttribute('data-testid')})))).filter(x=>/设置|setting/i.test(JSON.stringify(x))));
await page.screenshot({path:'design-demos/tools-settings/refs/magi-current.png'});
for(const [name,url] of [['raycast','https://manual.raycast.com/settings'],['vscode','https://code.visualstudio.com/docs/configure/extensions/extensions']]){
try {await page.goto(url,{waitUntil:'domcontentloaded',timeout:18000});await page.screenshot({path:`design-demos/tools-settings/refs/${name}.png`});console.log(name,'captured');}catch(e){console.log(name,e.message.slice(0,120));}
}
await browser.close();
