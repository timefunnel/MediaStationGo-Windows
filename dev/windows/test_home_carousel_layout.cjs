// Real CSS/browser regression for batched row geometry and pagination.
// HOME_CAROUSEL_BASELINE=1 reports the original per-row layout count.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {execFileSync} = require('node:child_process');
const {chromium} = require('playwright');

async function main() {
    const root=path.resolve(__dirname,'../..'), baseline=Boolean(process.env.HOME_CAROUSEL_BASELINE);
    const source=baseline ? execFileSync('git',['show','HEAD:src/web/mediastation.js'],{cwd:root,encoding:'utf8'}) : fs.readFileSync(path.join(root,'src/web/mediastation.js'),'utf8');
    const start=source.indexOf(baseline ? '    function createRowCarousel(' : '    const pendingCarouselLayouts');
    const carousel=source.slice(start,source.indexOf('    function createSection(',start));
    const css=fs.readFileSync(path.join(root,'src/web/mediastation.css'),'utf8');
    const browser=await chromium.launch({executablePath:process.env.VIDEO_UI_CHROME||'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true});
    try {
        const page=await browser.newPage({viewport:{width:1280,height:800},deviceScaleFactor:1.25});
        await page.setContent(`<style>${css}</style><main id="content" class="content" style="left:0"></main>`);
        await page.addScriptTag({content:`
            const content=document.getElementById('content');
            const clamp=(n,min,max)=>Math.max(min,Math.min(n,max));
            const element=(tag,kind)=>{const e=document.createElement(tag);e.className=kind;return e;};
            const scrollLimit=(node,axis)=>Math.max(0,axis==='x'?node.scrollWidth-node.clientWidth:node.scrollHeight-node.clientHeight);
            const stopSmoothScroll=()=>{};
            const horizontalScrollDurationMs=0;
            const smoothScrollTo=async(node,requested)=>{node.scrollLeft=requested.left;return true;};
            ${carousel}
            window.populate=()=>{
                for(let i=0;i<24;i++){
                    const row=element('div','media-row '+(i===22?'people-row':i===23?'episode-row':'home-media-row'+(i%2?' landscape':'')));
                    row.dataset.rowKey=String(i);
                    row.innerHTML='<button class="media-card"><span class="card-art"></span><span class="card-title">Cover</span></button>'.repeat(12);
                    content.append(createRowCarousel(row,'Test '+i,null,i===23?{initialScrollTarget:()=>300}:{}));
                }
            };
            window.refresh=refreshMediaRowCarousels;
            window.settle=()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
        `});
        const client=await page.context().newCDPSession(page);
        await client.send('Performance.enable');
        const metric=async()=>Object.fromEntries((await client.send('Performance.getMetrics')).metrics.map(e=>[e.name,e.value]));
        const before=await metric();
        await page.evaluate(async()=>{populate();await settle();});
        const after=await metric(), layoutCount=after.LayoutCount-before.LayoutCount;
        const layoutMs=(after.LayoutDuration-before.LayoutDuration)*1000;
        if(!baseline) assert.ok(layoutCount<=6,`initialization forced ${layoutCount} layouts for 24 rows`);
        assert.ok(await page.evaluate(()=>document.querySelector('[data-row-key="23"]').scrollLeft>290),'initial episode target lost');
        const state=()=>page.evaluate(()=>{
            const row=document.querySelector('[data-row-key="0"]'),shell=row.parentElement;
            return {left:row.scrollLeft,limit:row.scrollWidth-row.clientWidth,card:row.querySelector('.media-card').getBoundingClientRect().width,
                previous:shell.querySelector('.media-row-carousel-previous').disabled,next:shell.querySelector('.media-row-carousel-next').disabled};
        });
        assert.equal((await state()).previous,true);
        await page.evaluate(async()=>{document.querySelector('[data-row-key="0"]').parentElement.querySelector('.media-row-carousel-next').click();await settle();});
        const moved=await state();assert.ok(moved.left>100);assert.equal(moved.previous,false);
        await page.evaluate(async()=>{for(let i=0;i<15;i++){document.querySelector('[data-row-key="0"]').parentElement.querySelector('.media-row-carousel-next').click();await settle();}});
        const end=await state();assert.equal(end.next,true);assert.ok(Math.abs(end.left-end.limit)<2,'last page must align with row end');
        await page.setViewportSize({width:900,height:700});
        await page.evaluate(async()=>{refresh();refresh();await settle();});
        const resized=await state();assert.ok(resized.card>0);assert.ok(resized.left<=resized.limit+1);
        await page.evaluate(async()=>{content.style.display='none';refresh();await settle();});
        await page.evaluate(async()=>{content.style.display='';refresh();await settle();});
        assert.ok((await state()).card>0,'hidden player shell destroyed row metrics');
        await page.evaluate(async()=>{refresh();document.querySelector('[data-row-key="1"]').parentElement.remove();await settle();});
        console.log(JSON.stringify({result:'HOME_CAROUSEL_LAYOUT_OK',baseline,rows:24,layoutCount,layoutMs,pagination:true,resize:true,hidden:true}));
    } finally {await browser.close();}
}
main().catch(error=>{console.error(error);process.exitCode=1;});
