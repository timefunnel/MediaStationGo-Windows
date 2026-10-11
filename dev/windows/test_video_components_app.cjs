// Inspect the real embedded page in an explicitly launched isolated app smoke run.
const fs = require('node:fs');
const assert = require('node:assert/strict');
const path = require('node:path');
async function main() {
    const root = path.resolve(__dirname, '../..');
    const info = JSON.parse(fs.readFileSync(path.join(root, 'build/video-enhancement/smoke.json'), 'utf8').replace(/^\uFEFF/, ''));
    assert.ok(path.resolve(info.root).startsWith(path.join(root, 'build/video-enhancement-smoke-')));
    assert.ok(Number.isInteger(info.port) && info.port > 0 && info.port < 65536);
    const pages = await (await fetch(`http://127.0.0.1:${info.port}/json`)).json();
    const page = pages.find((p) => p.url === 'app://resources/mediastation.html');
    assert.ok(page, 'embedded app page missing');
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('CDP connection timeout')), 6000);
        ws.onopen = () => { clearTimeout(timer); resolve(); };
        ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP connection failed')); };
    });
    const expression = `new Promise((resolve,reject)=>{
        const id='component-smoke-'+Date.now();const previous=window._onMediaStationResponse;
        const timer=setTimeout(()=>{window._onMediaStationResponse=previous;reject(new Error('component IPC timeout'));},6000);
        window._onMediaStationResponse=(requestId,operation,ok,json)=>{
            if(requestId!==id){previous?.(requestId,operation,ok,json);return;}
            clearTimeout(timer);window._onMediaStationResponse=previous;
            if(!ok){reject(new Error('component status rejected'));return;}resolve(JSON.parse(json));
        };
        window.jmpNative.mediaStationFrameInterpolation(id,'video_components_status');
    })`;
    try {
        const result = await new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error('CDP evaluation timeout')), 10000);
            ws.onmessage = (event) => {
                const data = JSON.parse(event.data);
                if (data.id === 1) { clearTimeout(timer); resolve(data); }
            };
            ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
        });
        assert.ok(!result.error && !result.result.exceptionDetails, JSON.stringify(result.error || result.result.exceptionDetails));
        const nr = result.result.result.value.components.find((c) => c.id === 'dlssnr');
        assert.ok(nr.installed);
        assert.equal(nr.runtime.runtimeVerified, true);
        assert.equal(nr.runtime.playbackAvailable, true);
        assert.equal(nr.runtime.experimental, false);
        assert.equal(nr.runtime.defaults.nrTarget, 1080);
        const report = { appPage: page.url, component: 'dlssnr', installed: nr.installed, runtimeVerified: nr.runtime.runtimeVerified, playbackAvailable: nr.runtime.playbackAvailable, nrTarget: nr.runtime.defaults.nrTarget };
        fs.writeFileSync(path.join(root, 'build/video-enhancement/app-smoke-validation.json'), JSON.stringify(report, null, 2));
        console.log(`APP_COMPONENT_IPC_SMOKE_OK ${JSON.stringify(report)}`);
    } finally { ws.close(); }
}
main().catch((error) => { console.error(error.message); process.exitCode = 1; });
