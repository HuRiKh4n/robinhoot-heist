// Opens the page, plays one practice heist, clicks "Connect wallet" and records what happens.
import { chromium } from 'playwright';
const url = process.argv[2];
const b = await chromium.launch();
const p = await b.newPage({ viewport: { width: 390, height: 844 } });
const logs = [];
p.on('console', m => logs.push(`[${m.type()}] ${m.text()}`));
p.on('pageerror', e => logs.push(`[pageerror] ${e.message}`));
p.on('requestfailed', r => logs.push(`[requestfailed] ${r.url()} ${r.failure()?.errorText}`));
await p.goto(url, { waitUntil: 'networkidle' });
await p.click('#go'); await p.waitForTimeout(4000);
console.log('practice:', await p.textContent('#note'));
console.log('recent:', (await p.textContent('#recent')).replace(/\s+/g, ' ').slice(0, 200));
await p.click('#connect'); await p.waitForTimeout(6000);
await p.screenshot({ path: 'smoke-connect.png' });
console.log('note after connect:', await p.textContent('#note'));
console.log(logs.join('\n'));
await b.close();
