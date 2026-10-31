// Compare the real CEF viewport with a separately measured Win32 client rect.
// Use only an isolated, account-free app instance. --fixture replaces its UI.
// Usage: node test_native_window_geometry.cjs <port> <physical-w> <physical-h> <dpi> [--fixture]
const assert = require('node:assert/strict');

async function main() {
    const [port, physicalW, physicalH, dpi] = process.argv.slice(2, 6).map(Number);
    for (const value of [port, physicalW, physicalH, dpi]) assert.ok(Number.isInteger(value) && value > 0);
    assert.ok(port < 65536);
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    const target = pages.find(page => page.url === 'app://resources/mediastation.html');
    assert.ok(target, 'Isolated native app page is missing');
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    const pending = new Map();
    let nextId = 0;
    await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('CDP connection timeout')), 5000);
        ws.onopen = () => { clearTimeout(timer); resolve(); };
        ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP connection failed')); };
    });
    ws.onmessage = ({data}) => {
        const message = JSON.parse(data), task = pending.get(message.id);
        if (!task) return;
        pending.delete(message.id);
        clearTimeout(task.timer);
        if (message.error || message.result?.exceptionDetails) task.reject(new Error('CDP evaluation failed'));
        else task.resolve(message.result.result.value);
    };
    const evaluate = expression => new Promise((resolve, reject) => {
        const id = ++nextId;
        const timer = setTimeout(() => { pending.delete(id); reject(new Error('CDP evaluation timeout')); }, 5000);
        pending.set(id, {resolve, reject, timer});
        ws.send(JSON.stringify({id, method: 'Runtime.evaluate', params: {expression, returnByValue: true, awaitPromise: true}}));
    });
    try {
        if (process.argv.includes('--fixture')) {
            await evaluate(`(() => {
                document.title = 'MediaStationGo · 布局验证';
                document.head.querySelectorAll('style,link[rel="stylesheet"]').forEach(node => node.remove());
                document.body.innerHTML = '<style>html,body{margin:0;width:100%;height:100%;overflow:hidden;background:#132234;color:#fff;font:20px sans-serif}main{box-sizing:border-box;position:fixed;inset:0;border:6px solid #64d3a3;display:grid;place-items:center;background:linear-gradient(135deg,#132234,#284563)}button{position:fixed;right:16px;bottom:16px;width:180px;height:64px;border:0;border-radius:12px;background:#64d3a3;color:#132234;font:20px sans-serif}output{white-space:pre-line;text-align:center}</style><main><output id="geometry-label"></output><button id="geometry-corner">点击右下角</button></main>';
                window.geometryClicks = [];
                window.addEventListener('pointerdown', event => window.geometryClicks.push({x:event.clientX,y:event.clientY,target:event.target.id}));
                const label = () => document.getElementById('geometry-label').textContent = '原生窗口尺寸验证\\n'+innerWidth+' × '+innerHeight+' CSS px\\nDPR '+devicePixelRatio;
                window.addEventListener('resize', label); label();
                document.getElementById('geometry-corner').onclick = event => event.currentTarget.textContent = '点击位置正确';
                return true;
            })()`);
        }
        const state = await evaluate(`({width:innerWidth,height:innerHeight,dpr:devicePixelRatio,clicks:window.geometryClicks||[]})`);
        // Rounding a logical pixel can differ from physical/DPI by < 1 px.
        assert.ok(Math.abs(state.width * dpi / 96 - physicalW) <= dpi / 96, `CEF width differs from native client: ${JSON.stringify(state)}`);
        assert.ok(Math.abs(state.height * dpi / 96 - physicalH) <= dpi / 96, `CEF height differs from native client: ${JSON.stringify(state)}`);
        assert.ok(Math.abs(state.dpr - dpi / 96) < 0.005, 'CEF screen scale differs from native DPI');
        console.log(JSON.stringify({result:'NATIVE_WINDOW_GEOMETRY_OK',physicalW,physicalH,dpi,...state}));
    } finally {
        ws.close();
    }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
