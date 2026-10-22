// Attach to an explicitly enabled local CEF debugging port. The user supplies input.
// node dev/windows/profile_scroll_latency.cjs <port> <report-directory> [exe]
// Type stop + Enter, or create <report-directory>/stop, to finish.
// SCROLL_TRACE=1 also saves a Chromium trace (can be large).
// This script never closes the application. Event samples omit resource URLs;
// optional Chromium traces may contain URLs and stay in the local report folder.
const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const { createHash } = require('node:crypto');

async function main() {
    const port = Number(process.argv[2]);
    assert.ok(Number.isInteger(port) && port > 0 && port < 65536, 'invalid local debugging port');
    const directory = path.resolve(process.argv[3]);
    fs.mkdirSync(directory, { recursive: true });
    assert.ok(!fs.existsSync(path.join(directory,'session.json')), 'report directory already contains a session');
    let child;
    if (process.argv[4]) {
        const exe = path.resolve(process.argv[4]);
        child = spawn(exe, ['--remote-debug-port',String(port),'--log-file',path.join(directory,'app.log'),'--log-level','info,scroll_latency=trace'], {cwd:path.dirname(exe),windowsHide:true,stdio:'ignore'});
        child.on('error',error=>{console.error(error);process.exitCode=1;});
        fs.writeFileSync(path.join(directory,'binary.json'),JSON.stringify({pid:child.pid,exe,sha256:createHash('sha256').update(fs.readFileSync(exe)).digest('hex')},null,2));
    }
    let page;
    console.log(`WAITING_FOR_APPLICATION port=${port}`);
    for (let attempt = 0; attempt < 2400; attempt++) {
        try {
            const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
            page = pages.find(item => item.url === 'app://resources/mediastation.html');
        } catch (error) {
            if (error.cause?.code !== 'ECONNREFUSED') throw error;
        }
        if (page) break;
        assert.ok(!child || child.exitCode === null, `application exited before diagnostic attach (${child?.exitCode})`);
        await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.ok(page, 'embedded application page missing');
    const samples = fs.createWriteStream(path.join(directory, 'scroll-events.jsonl'));
    const trace = process.env.SCROLL_TRACE === '1'
        ? fs.createWriteStream(path.join(directory, 'chromium-trace.json')) : null;
    trace?.write('{"traceEvents":[');
    let sequence = 0, firstTrace = true, traceCount = 0, finished = false;
    const pending = new Map(), counts = {};
    let traceComplete;
    const completed = new Promise(resolve => { traceComplete = resolve; });
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
    const send = (method, params = {}) => new Promise((resolve, reject) => {
        const id = ++sequence;
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`${method} timeout`)); }, 15000);
        pending.set(id, { resolve, reject, timer });
        ws.send(JSON.stringify({ id, method, params }));
    });
    const evaluate = async expression => {
        const result = await send('Runtime.evaluate', { expression, returnByValue: true });
        assert.ok(!result.exceptionDetails, JSON.stringify(result.exceptionDetails));
        return result.result.value;
    };
    ws.onmessage = ({ data }) => {
        const message = JSON.parse(data);
        if (message.method === 'Tracing.dataCollected') {
            for (const event of message.params.value) {
                trace?.write(`${firstTrace ? '' : ','}${JSON.stringify(event)}`);
                firstTrace = false;
                traceCount++;
            }
        } else if (message.method === 'Tracing.tracingComplete') {
            traceComplete();
        } else if (message.method === 'Runtime.bindingCalled' && message.params.name === '__scrollDiagnosticSink') {
            const batch = JSON.parse(message.params.payload);
            for (const event of batch) {
                counts[event.kind] = (counts[event.kind] || 0) + 1;
                samples.write(`${JSON.stringify(event)}\n`);
            }
        }
        const request = pending.get(message.id);
        if (!request) return;
        pending.delete(message.id);
        clearTimeout(request.timer);
        if (message.error) request.reject(new Error(message.error.message));
        else request.resolve(message.result);
    };
    await send('Runtime.enable');
    await send('Runtime.addBinding', { name: '__scrollDiagnosticSink' });
    if (trace) await send('Tracing.start', {
        categories: 'input,latencyInfo,benchmark,cc,devtools.timeline',
        transferMode: 'ReportEvents', bufferUsageReportingInterval: 1000,
        options: 'record-continuously',
    });
    const metadata = await evaluate(`(() => {
        if (window.__scrollDiagnostic) throw new Error('scroll diagnostic already running');
        const controller = new AbortController(), ids = new WeakMap();
        let nextId = 0, buffer = [], activeUntil = 0, frame = 0;
        const scrollers = new Set();
        const describe = element => {
            if (!ids.has(element)) ids.set(element, ++nextId);
            return {id:ids.get(element),tag:element.tagName,classes:String(element.className || '').slice(0,160)};
        };
        const record = (kind, data) => buffer.push({kind,t:performance.now(),...data});
        const flush = () => { if (buffer.length) { __scrollDiagnosticSink(JSON.stringify(buffer)); buffer = []; } };
        const tick = t => {
            record('frame', {raf:t,positions:[...scrollers].filter(e=>e.isConnected).map(e=>({...describe(e),top:e.scrollTop,left:e.scrollLeft}))});
            if (performance.now() < activeUntil) frame = requestAnimationFrame(tick);
            else frame = 0;
        };
        const activate = () => { activeUntil = performance.now() + 900; if (!frame) frame = requestAnimationFrame(tick); };
        window.addEventListener('wheel', event => {
            let element = event.target instanceof Element ? event.target : document.documentElement;
            const targets = [];
            for (let e=element;e;e=e.parentElement) {
                if (e.scrollHeight > e.clientHeight + 1 || e.scrollWidth > e.clientWidth + 1) {
                    scrollers.add(e); targets.push({...describe(e),top:e.scrollTop,left:e.scrollLeft});
                }
            }
            record('wheel',{eventTime:event.timeStamp,delay:performance.now()-event.timeStamp,dx:event.deltaX,dy:event.deltaY,mode:event.deltaMode,trusted:event.isTrusted,targets});
            activate();
        }, {capture:true,passive:true,signal:controller.signal});
        window.addEventListener('scroll', event => {
            const e = event.target === document ? document.scrollingElement : event.target;
            if (!(e instanceof Element)) return;
            scrollers.add(e); record('scroll',{...describe(e),top:e.scrollTop,left:e.scrollLeft}); activate();
        }, {capture:true,passive:true,signal:controller.signal});
        const observer = new PerformanceObserver(list => { for (const e of list.getEntries()) record('longtask',{start:e.startTime,duration:e.duration}); });
        observer.observe({type:'longtask',buffered:true});
        const resources = new PerformanceObserver(list => { for (const e of list.getEntries()) record('resource',{initiator:e.initiatorType,start:e.startTime,duration:e.duration,end:e.responseEnd,bytes:e.transferSize}); });
        resources.observe({type:'resource',buffered:true});
        const paint = new PerformanceObserver(list => { for (const e of list.getEntries()) record('paint',{name:e.name,start:e.startTime}); });
        paint.observe({type:'paint',buffered:true});
        const timer = setInterval(flush,500);
        window.__scrollDiagnostic = {stop(){controller.abort();observer.disconnect();resources.disconnect();paint.disconnect();clearInterval(timer);cancelAnimationFrame(frame);flush();delete window.__scrollDiagnostic;}};
        return {timeOrigin:performance.timeOrigin,started:performance.now(),dpr:devicePixelRatio,viewport:[innerWidth,innerHeight],cards:document.querySelectorAll('.media-card').length,userAgent:navigator.userAgent};
    })()`);
    fs.writeFileSync(path.join(directory, 'session.json'), JSON.stringify({port,startedAt:new Date().toISOString(),...metadata},null,2));
    console.log(`SCROLL_LOG_READY ${JSON.stringify({directory,...metadata})}`);
    const input = readline.createInterface({ input: process.stdin });
    let stopTimer;
    const stop = new Promise(resolve => {
        input.on('line', line => { if (line.trim() === 'stop') resolve(); });
        process.once('SIGINT', resolve);
        stopTimer = setInterval(() => { if (fs.existsSync(path.join(directory,'stop'))) resolve(); },250);
    });
    const closed = new Promise(resolve => ws.addEventListener('close', resolve, {once:true}));
    const status = setInterval(() => console.log(JSON.stringify({counts,traceCount})),10000);
    await Promise.race([stop,closed]);
    clearInterval(status);
    clearInterval(stopTimer);
    if (ws.readyState === WebSocket.OPEN) {
        await evaluate('window.__scrollDiagnostic?.stop()');
        if (trace) {
            await send('Tracing.end');
            await completed;
        }
        finished = true;
        ws.close();
    }
    input.close();
    trace?.end(']}'); samples.end();
    await Promise.all([
        ...(trace ? [new Promise(resolve=>trace.on('finish',resolve))] : []),
        new Promise(resolve=>samples.on('finish',resolve)),
    ]);
    fs.writeFileSync(path.join(directory,'result.json'),JSON.stringify({finished,counts,traceCount,endedAt:new Date().toISOString()},null,2));
    console.log(`SCROLL_LOG_SAVED ${JSON.stringify({finished,counts,traceCount})}`);
}
main().catch(error => { console.error(error); process.exitCode=1; });
