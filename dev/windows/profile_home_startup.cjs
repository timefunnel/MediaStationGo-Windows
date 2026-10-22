// Profile an actual embedded home page with a private cache. No media is played.
// node profile_home_startup.cjs <exe> <label> [existing-private-cache]
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const assert = require('node:assert/strict');
const { spawn, execFileSync } = require('node:child_process');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function main() {
    const exe = path.resolve(process.argv[2]), label = process.argv[3];
    assert.match(label, /^[a-z0-9-]+$/);
    const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'msg-profile-'));
    const config = path.join(temp, 'config'), cache = process.argv[4] ? path.resolve(process.argv[4]) : path.join(temp, 'cache');
    await fs.mkdir(config);
    await fs.copyFile(process.env.HOME_PROFILE_SETTINGS || path.join(process.env.APPDATA, 'jellium-desktop/settings.json'), path.join(config, 'settings.json'));
    const reservation = net.createServer();
    await new Promise(resolve => reservation.listen(0, '127.0.0.1', resolve));
    const port = reservation.address().port;
    await new Promise(resolve => reservation.close(resolve));
    let child, ws, sequence = 0;
    const pending = new Map();
    const scripts = new Map();
    let traceDone;
    const traceCompleted = new Promise(resolve => { traceDone = resolve; });
    const send = (method, params = {}) => new Promise((resolve, reject) => {
        const id = ++sequence, timer = setTimeout(() => { pending.delete(id); reject(new Error(`${method} timeout`)); }, 10000);
        pending.set(id, { resolve, reject, timer }); ws.send(JSON.stringify({ id, method, params }));
    });
    const evaluate = async expression => {
        const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
        assert.ok(!result.exceptionDetails, 'profile expression failed'); return result.result.value;
    };
    const reportDir = path.resolve(__dirname, '../../build/video-enhancement/vfg-black-screen');
    await fs.mkdir(reportDir, { recursive: true });
    try {
        child = spawn(exe, ['--config-dir', config, '--cache-dir', cache, '--remote-debug-port', String(port), '--log-file', path.join(temp, 'app.log')], { cwd: path.dirname(exe), windowsHide: true, stdio: 'ignore' });
        await fs.writeFile(path.join(reportDir, 'profile-active.json'), JSON.stringify({ pid: child.pid, port, label, cache }));
        let page;
        for (let n = 0; n < 200; n++) {
            try { page = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find(p => p.url === 'app://resources/mediastation.html'); }
            catch (error) { if (error.cause?.code !== 'ECONNREFUSED') throw error; }
            if (page) break; await delay(50);
        }
        assert.ok(page, 'embedded page missing');
        ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('CDP connection failed')); });
        ws.onmessage = ({ data }) => { const message = JSON.parse(data); if(message.method==='Tracing.tracingComplete')traceDone(message.params.stream); if(message.method==='Debugger.scriptParsed')scripts.set(message.params.url,message.params.scriptId); const request = pending.get(message.id); if (!request) return; pending.delete(message.id); clearTimeout(request.timer); if (message.error) request.reject(new Error(message.error.message)); else request.resolve(message.result); };
        await send('Debugger.enable');
        await send('Tracing.start', { categories:'devtools.timeline,disabled-by-default-devtools.timeline,cc,gpu,disabled-by-default-gpu.service', transferMode:'ReturnAsStream' });
        await send('Profiler.enable'); await send('Profiler.setSamplingInterval', { interval: 1000 }); await send('Profiler.start');
        await evaluate(`(()=>{
            const p=window.__homeProfile={started:performance.now(),longTasks:[],frames:[],responses:[],rebuilds:[],scrolls:[]};
            new PerformanceObserver(list=>list.getEntries().forEach(e=>p.longTasks.push({start:e.startTime,duration:e.duration}))).observe({type:'longtask',buffered:true});
            let last=performance.now(); function frame(t){p.frames.push({t,delta:t-last});last=t;requestAnimationFrame(frame);}requestAnimationFrame(frame);
            let handler=window._onMediaStationResponse;
            Object.defineProperty(window,'_onMediaStationResponse',{configurable:true,get(){return (id,op,ok,json)=>{const start=performance.now();try{return handler?.(id,op,ok,json);}finally{p.responses.push({start,op,ok,bytes:json.length,duration:performance.now()-start});}};},set(value){handler=value;}});
            const timer=setInterval(()=>{const c=document.getElementById('content');if(!c)return;clearInterval(timer);new MutationObserver(records=>{for(const r of records)if(r.target===c)p.rebuilds.push({t:performance.now(),added:r.addedNodes.length,removed:r.removedNodes.length});}).observe(c,{childList:true});c.addEventListener('scroll',()=>p.scrolls.push({t:performance.now(),top:c.scrollTop}),{passive:true});},10);
            return true;
        })()`);
        console.log(`PROFILE_READY ${JSON.stringify({ label, port, pid: child.pid })}`);
        await delay(Number(process.env.PROFILE_SECONDS || 45) * 1000);
        const data = await evaluate(`({ ...window.__homeProfile, cache:document.getElementById('app-shell').dataset.initialHomeCache, cards:document.querySelectorAll('.media-card').length, images:document.querySelectorAll('.image-ready').length, dpr:devicePixelRatio,viewport:[innerWidth,innerHeight], total:performance.now() })`);
        const contentObject=await send('Runtime.evaluate',{expression:"document.getElementById('content')"});
        const {listeners}=await send('DOMDebugger.getEventListeners',{objectId:contentObject.result.objectId});
        data.blockingWheelListeners=listeners.filter(e=>e.type==='wheel'&&!e.passive).length;
        const scriptId=scripts.get('app://resources/mediastation.js');
        assert.ok(scriptId,'embedded frontend script missing');
        const {scriptSource:loadedSource}=await send('Debugger.getScriptSource',{scriptId});
        data.localSourceMatches=loadedSource===await fs.readFile(path.resolve(__dirname,'../../src/web/mediastation.js'),'utf8');
        assert.ok(data.cards>0,'home did not load');
        if(label.startsWith('final-')) {
            assert.equal(data.blockingWheelListeners,0,'embedded page still blocks wheel scrolling');
            assert.equal(data.localSourceMatches,true,'embedded frontend differs from the tested source');
        }
        const { profile } = await send('Profiler.stop');
        await send('Tracing.end');
        const stream = await traceCompleted;
        let traceText = '';
        for (;;) { const chunk=await send('IO.read',{handle:stream}); traceText += chunk.base64Encoded ? Buffer.from(chunk.data,'base64').toString('utf8') : chunk.data; if(chunk.eof)break; }
        await send('IO.close',{handle:stream});
        const traceTotals = new Map();
        data.traceSlow=[];
        for(const e of JSON.parse(traceText).traceEvents) {
            if(e.ph!=='X' || !e.dur)continue;
            const key=e.name, stats=traceTotals.get(key)||{name:key,count:0,totalMs:0,maxMs:0};
            stats.count++;stats.totalMs+=e.dur/1000;stats.maxMs=Math.max(stats.maxMs,e.dur/1000);traceTotals.set(key,stats);
            if(e.dur>10000)data.traceSlow.push({name:e.name,durationMs:e.dur/1000,pid:e.pid,tid:e.tid,stack:(e.args?.beginData?.stackTrace||[]).filter(f=>f.url?.startsWith('app:')).map(f=>({name:f.functionName,line:f.lineNumber}))});
        }
        data.traceTop=[...traceTotals.values()].sort((a,b)=>b.totalMs-a.totalMs).slice(0,45);
        const nodes = new Map(profile.nodes.map(n => [n.id, n]));
        const totals = new Map();
        profile.samples.forEach((id, i) => { const f = nodes.get(id).callFrame; const key = `${f.functionName || '(anonymous)'}@${f.url.startsWith('app:') ? f.url : '(native)'}:${f.lineNumber + 1}`; totals.set(key, (totals.get(key) || 0) + (profile.timeDeltas[i] || 0)); });
        data.cpuTop = [...totals].map(([name, us]) => ({ name, ms: us / 1000 })).sort((a,b) => b.ms-a.ms).slice(0,30);
        data.label = label;
        await fs.writeFile(path.join(reportDir, `home-${label}.json`), JSON.stringify(data, null, 2));
        const gaps = data.frames.map(f=>f.delta).sort((a,b)=>a-b);
        console.log(JSON.stringify({ label, cache:data.cache,cards:data.cards,images:data.images,longTasks:data.longTasks,frameP99:gaps[Math.floor(gaps.length*.99)],maxFrame:gaps.at(-1),rebuilds:data.rebuilds,scrollEvents:data.scrolls.length,cpuTop:data.cpuTop.slice(0,12) }));
        // Preserve this private cache for a second, warm-cache run.
        await fs.writeFile(path.join(reportDir, `home-${label}-cache.json`), JSON.stringify({ temp, cache }));
    } finally {
        ws?.close(); for (const r of pending.values()) clearTimeout(r.timer);
        if (child && child.exitCode === null) execFileSync('taskkill', ['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
    }
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
