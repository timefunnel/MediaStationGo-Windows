// Read-only capability check against the standard app's real native IPC.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const port = Number(process.argv[2]);
assert.ok(Number.isInteger(port) && port > 0 && port < 65536, 'debug port required');
async function main() {
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    const page = pages.find((entry) => entry.url === 'app://resources/mediastation.html');
    assert.ok(page, 'embedded page missing');
    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('CDP connection timeout')), 6000);
        ws.onopen = () => { clearTimeout(timer); resolve(); };
        ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP connection failed')); };
    });
    try {
        const response = await new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error('native IPC timeout')), 10000);
            ws.onmessage = ({ data }) => { const message = JSON.parse(data); if (message.id === 1) { clearTimeout(timer); resolve(message); } };
            const expression = `new Promise((resolve,reject)=>{
                const id='dynamic-hdr-status-'+Date.now(),previous=window._onMediaStationResponse;
                const timer=setTimeout(()=>{window._onMediaStationResponse=previous;reject(new Error('IPC timeout'));},6000);
                window._onMediaStationResponse=(requestId,operation,ok,json)=>{
                    if(requestId!==id){previous?.(requestId,operation,ok,json);return;}
                    clearTimeout(timer);window._onMediaStationResponse=previous;
                    if(!ok){reject(new Error('IPC rejected'));return;}
                    resolve(JSON.parse(json).components.filter(c=>c.id!=='rife').map(c=>({id:c.id,installed:c.installed,installPath:c.installPath,runtime:c.runtime})));
                };
                window.jmpNative.mediaStationFrameInterpolation(id,'video_components_status');
            })`;
            ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
        });
        assert.ok(!response.error && !response.result?.exceptionDetails, 'embedded native capability query failed');
        const components = response.result.result.value;
        assert.equal(components.length, 3);
        for (const component of components) {
            assert.deepEqual(component.runtime.supportedDynamicRanges, ['sdr', 'hdr10', 'hlg', 'hdr10plus', 'dolbyvision']);
            assert.equal(component.runtime.dolbyVisionProcessing, 'fel-reconstruction-rpu-color-mapping-and-scene-metadata');
            assert.equal(component.runtime.dolbyVisionEnhancementLayer, true);
        }
        const report = { page: page.url, mediaStarted: false, components };
        fs.writeFileSync(path.resolve(__dirname, '../../build/video-enhancement/dynamic-hdr-app-ipc.json'), JSON.stringify(report, null, 2));
        console.log(`DYNAMIC_HDR_APP_IPC_OK components=${components.map((c) => `${c.id}:${c.installed ? 'installed' : 'not-installed'}`).join(',')}`);
    } finally { ws.close(); }
}
main().catch((error) => { console.error(error.message); process.exitCode = 1; });
