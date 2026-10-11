// Run immediately after launching the standard app with --remote-debug-port.
// Checks existing user installs without opening component management or starting media.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const port = Number(process.argv[2]);
const expected = process.argv.slice(3);
assert.ok(Number.isInteger(port) && port > 0 && port < 65536, 'debug port required');
assert.ok(expected.length && expected.every((id) => ['rtx-video', 'dlssnr', 'nvidia-vfg'].includes(id)), 'installed component ids required');
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
    let requestId = 0;
    const pending = new Map();
    ws.onmessage = ({ data }) => {
        const response = JSON.parse(data), request = pending.get(response.id);
        if (!request) return;
        pending.delete(response.id); clearTimeout(request.timer);
        if (response.error || response.result?.exceptionDetails) request.reject(new Error('Embedded evaluation failed'));
        else request.resolve(response.result.result.value);
    };
    function evaluate(expression) {
        return new Promise((resolve, reject) => {
            const id = ++requestId;
            const timer = setTimeout(() => { pending.delete(id); reject(new Error('Evaluation timeout')); }, 10000);
            pending.set(id, { resolve, reject, timer });
            ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
        });
    }
    try {
        const expression = `({managementHidden:document.getElementById('components-page').classList.contains('hidden'),statuses:${JSON.stringify(expected)}.map(id=>({id,text:document.getElementById('component-'+id+'-status').textContent,state:document.getElementById('component-'+id+'-status').dataset.state}))})`;
        let firstEntry;
        const deadline = Date.now() + 18000;
        while (Date.now() < deadline) {
            firstEntry = await evaluate(expression);
            assert.equal(firstEntry.managementHidden, true, 'component management must stay unopened');
            if (firstEntry.statuses.every((status) => status.state === 'ready' && /^已安装/.test(status.text))) break;
            await new Promise((resolve) => setTimeout(resolve, 250));
        }
        assert.ok(firstEntry.statuses.every((status) => status.state === 'ready' && /^已安装/.test(status.text)), 'home entry did not load installed NVIDIA component state');
        const native = await evaluate(`new Promise((resolve,reject)=>{
            const id='first-entry-check-'+Date.now(),previous=window._onMediaStationResponse;
            const timer=setTimeout(()=>{window._onMediaStationResponse=previous;reject(new Error('IPC timeout'));},6000);
            window._onMediaStationResponse=(requestId,operation,ok,json)=>{
                if(requestId!==id){previous?.(requestId,operation,ok,json);return;}
                clearTimeout(timer);window._onMediaStationResponse=previous;
                if(!ok){reject(new Error('IPC rejected'));return;}
                resolve(JSON.parse(json).components.map(c=>({id:c.id,installed:c.installed,playbackAvailable:c.runtime?.playbackAvailable===true})));
            };
            window.jmpNative.mediaStationFrameInterpolation(id,'video_components_status');
        })`);
        for (const id of expected) {
            const status = native.find((entry) => entry.id === id);
            assert.ok(status?.installed && status.playbackAvailable, `${id} not ready in native runtime`);
        }
        const report = { componentManagementOpened: false, firstHomeEntryLoaded: true, uiStatuses: firstEntry.statuses, nativeStatuses: native, mediaStarted: false };
        fs.writeFileSync(path.resolve(__dirname, '../../build/video-enhancement/component-detection-first-entry.json'), JSON.stringify(report, null, 2));
        console.log(`APP_COMPONENT_FIRST_ENTRY_OK ${JSON.stringify(report)}`);
    } finally {
        for (const request of pending.values()) clearTimeout(request.timer);
        ws.close();
    }
}
main().catch((error) => { console.error(error.message); process.exitCode = 1; });
