import {chromium} from '/Users/xie/.npm/_npx/e41f203b7505f1fb/node_modules/playwright/index.mjs';
const b=await chromium.launch({executablePath:'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true}); const p=await b.newPage({viewport:{width:1440,height:1000}});
await p.goto('http://127.0.0.1:38123/web.html');await p.getByTestId('sidebar-settings').click();await p.getByTestId('settings-nav-tools').click();await p.waitForTimeout(700);
await p.screenshot({path:'design-demos/tools-settings/refs/magi-settings.png'});
console.log(await p.locator('.settings-page').evaluate(el=>{const s=getComputedStyle(el);return {text:el.textContent.slice(0,2800),tokens:Object.fromEntries(['--background','--foreground','--foreground-muted','--border','--primary','--font-family','--magi-surface-sidebar','--magi-surface-main','--btn-primary-bg','--magi-hairline'].map(k=>[k,s.getPropertyValue(k)])),navWidth:el.querySelector('aside')?.getBoundingClientRect().width}}));await b.close();
