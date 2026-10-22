// Uses the real stylesheet and any content wheel handler from the selected JS.
// HOME_SCROLL_BASELINE=1 runs the same behavior checks against HEAD (must fail).
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { chromium } = require('playwright');
async function main() {
    const root=path.resolve(__dirname,'../..');
    const source=process.env.HOME_SCROLL_BASELINE ? execFileSync('git',['show','HEAD:src/web/mediastation.js'],{cwd:root,encoding:'utf8'}) : fs.readFileSync(path.join(root,'src/web/mediastation.js'),'utf8');
    const start=source.indexOf('    function wheelPixels(');
    const wheelHandler=start<0 ? '' : source.slice(start,source.indexOf('    const rowScrollIdleTimers',start));
    const css=fs.readFileSync(path.join(root,'src/web/mediastation.css'),'utf8');
    const browser=await chromium.launch({executablePath:process.env.VIDEO_UI_CHROME||'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
    try {
        const page=await browser.newPage({viewport:{width:1280,height:800}});
        await page.setContent(`<style>${css}</style><main id="content" class="content" style="left:0"><div style="height:80px"></div><div class="media-row">${'<button class="media-card" style="height:180px">cover</button>'.repeat(12)}</div><div style="height:1800px"></div></main><script>const content=document.getElementById('content');${wheelHandler}</script>`);
        const client=await page.context().newCDPSession(page);
        const object=await client.send('Runtime.evaluate',{expression:"document.getElementById('content')"});
        const {listeners}=await client.send('DOMDebugger.getEventListeners',{objectId:object.result.objectId});
        const blocking=listeners.filter(e=>e.type==='wheel'&&!e.passive).length;
        await page.locator('.media-row').hover();
        await page.mouse.wheel(0,300);
        await page.waitForFunction(()=>document.getElementById('content').scrollTop>100);
        await page.evaluate(()=>document.getElementById('content').scrollTop=0);
        await page.locator('.media-row').hover();
        await page.mouse.wheel(450,0);
        await page.waitForFunction(()=>document.querySelector('.media-row').scrollLeft>100,{},{timeout:2000});
        assert.equal(blocking,0,'home scrolling must not wait for a blocking wheel handler');
        console.log('HOME_SCROLL_BROWSER_OK native vertical/horizontal scrolling; blocking wheel listeners=0');
    } finally {await browser.close();}
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
