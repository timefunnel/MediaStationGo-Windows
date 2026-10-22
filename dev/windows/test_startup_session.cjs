// Requires an existing saved account. Uses a private config/cache and a local
// non-forwarding proxy; never logs credentials or contacts the media server.
// node test_startup_session.cjs <exe> [baseline]
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const net = require('node:net');
const { spawn, execFileSync } = require('node:child_process');
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function listen(server) {
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
    return server.address().port;
}

async function main() {
    const exe = path.resolve(process.argv[2]);
    const baseline = process.argv[3] === 'baseline';
    const temp = await fs.mkdtemp(path.join(os.tmpdir(), 'msg-startup-'));
    const config = path.join(temp, 'config'), cache = path.join(temp, 'cache');
    await fs.mkdir(config);
    const settingsPath = process.env.HOME_PROFILE_SETTINGS || path.join(process.env.APPDATA, 'jellium-desktop/settings.json');
    const settings = JSON.parse(await fs.readFile(settingsPath, 'utf8'));
    settings.mediaStationProxyMode = 'system';
    await fs.writeFile(path.join(config, 'settings.json'), JSON.stringify(settings));
    const sockets = new Set();
    let connections = 0, release = false, child, ws;
    const proxy = net.createServer((socket) => {
        connections += 1;
        sockets.add(socket);
        socket.on('error', () => {});
        socket.on('close', () => sockets.delete(socket));
        // Do not read or forward the request, which may contain credentials.
        if (release) socket.destroy();
    });
    const proxyPort = await listen(proxy);
    const reservation = net.createServer();
    const port = await listen(reservation);
    await new Promise((resolve) => reservation.close(resolve));
    let sequence = 0;
    const pending = new Map();
    function evaluate(expression) {
        return new Promise((resolve, reject) => {
            const id = ++sequence;
            const timer = setTimeout(() => { pending.delete(id); reject(new Error('CDP evaluation timeout')); }, 5000);
            pending.set(id, { resolve, reject, timer });
            ws.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
        });
    }
    const started = Date.now();
    const releaseTimer = setTimeout(() => { release = true; for (const socket of sockets) socket.destroy(); }, 8000);
    try {
        const proxyUrl = `http://127.0.0.1:${proxyPort}`;
        child = spawn(exe, ['--config-dir', config, '--cache-dir', cache, '--remote-debug-port', String(port), '--log-file', path.join(temp, 'app.log')], {
            cwd: path.dirname(exe), windowsHide: true, stdio: 'ignore',
            env: { ...process.env, HTTP_PROXY: proxyUrl, HTTPS_PROXY: proxyUrl, ALL_PROXY: proxyUrl, NO_PROXY: '', http_proxy: proxyUrl, https_proxy: proxyUrl, no_proxy: '' },
        });
        await new Promise((resolve, reject) => { child.once('spawn', resolve); child.once('error', reject); });
        let page;
        while (Date.now() - started < 20000) {
            let pages = [];
            try { pages = await (await fetch(`http://127.0.0.1:${port}/json`, { signal: AbortSignal.timeout(400) })).json(); }
            catch (error) { if (error.name !== 'TimeoutError' && error.cause?.code !== 'ECONNREFUSED') throw error; }
            page = pages.find((entry) => entry.url === 'app://resources/mediastation.html');
            if (page) break;
            assert.equal(child.exitCode, null, 'app exited during startup');
            await delay(80);
        }
        assert.ok(page, 'embedded page did not appear');
        const pageMs = Date.now() - started;
        ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('CDP connection failed')); });
        ws.onmessage = ({ data }) => {
            const message = JSON.parse(data), request = pending.get(message.id);
            if (!request) return;
            pending.delete(message.id); clearTimeout(request.timer);
            if (message.error || message.result?.exceptionDetails) request.reject(new Error('Embedded evaluation failed'));
            else request.resolve(message.result.result.value);
        };
        for (let n = 0; n < 60; n += 1) {
            if (await evaluate(`Boolean(document.getElementById('login-view'))`)) break;
            await delay(50);
        }
        if (baseline) {
            assert.ok(pageMs >= 7000, `baseline unexpectedly created its page in ${pageMs} ms`);
        } else {
            assert.ok(pageMs < 5000, `page creation was blocked for ${pageMs} ms`);
            while (!connections && Date.now() - started < 6000) await delay(50);
            assert.ok(connections > 0, 'saved session did not reach the controlled proxy');
            const splash = await evaluate(`(async()=>({visible:!document.getElementById('splash').classList.contains('hidden'),frames:await new Promise(resolve=>{let n=0;function tick(){if(++n===5)resolve(n);else requestAnimationFrame(tick);}requestAnimationFrame(tick);})}))()`);
            assert.ok(splash.visible && splash.frames === 5, 'splash did not remain responsive during restoration');
        }
        while (!release) await delay(50);
        let gateway = false;
        for (let n = 0; n < 100; n += 1) {
            gateway = await evaluate(`!document.getElementById('login-view').classList.contains('hidden')`);
            if (gateway) break;
            await delay(100);
        }
        assert.ok(gateway, 'failed restoration did not show account selection');
        if (!baseline) {
            // The gateway opens before its asynchronous saved-account list
            // completes; its error is presented only after that request.
            let explained = false;
            for (let n = 0; n < 100; n += 1) {
                explained = await evaluate(`document.body.innerText.includes('无法恢复登录状态，请选择账号重新连接')`);
                if (explained) break;
                await delay(50);
            }
            assert.equal(explained, true, 'restoration failure was not explained');
            const previousConnections = connections;
            const status = await evaluate(`new Promise(resolve=>{const previous=window._onMediaStationResponse;window._onMediaStationResponse=(id,op,ok,json)=>{if(id!=='startup-repeat'){previous(id,op,ok,json);return;}window._onMediaStationResponse=previous;resolve({ok,code:JSON.parse(json).code});};window.jmpNative.mediaStationSessionStatus('startup-repeat');})`);
            assert.deepEqual(status, { ok: false, code: 'session_restore_failed' });
            assert.equal(connections, previousConnections, 'status retried startup restoration');
        }
        assert.ok(connections > 0, 'test did not exercise network restoration');
        const report = { baseline, pageMs, controlledProxyConnections: connections, gatewayVisible: gateway, startupUnblocked: !baseline };
        const reportDir = path.resolve(__dirname, '../../build/video-enhancement/vfg-black-screen');
        await fs.mkdir(reportDir, { recursive: true });
        await fs.writeFile(path.join(reportDir, baseline ? 'startup-baseline.json' : 'startup-regression.json'), JSON.stringify(report, null, 2));
        console.log(JSON.stringify(report));
    } finally {
        clearTimeout(releaseTimer);
        ws?.close();
        for (const request of pending.values()) clearTimeout(request.timer);
        if (child && child.exitCode === null) execFileSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
        for (const socket of sockets) socket.destroy();
        await new Promise((resolve) => proxy.close(resolve));
        assert.ok(temp.startsWith(path.join(os.tmpdir(), 'msg-startup-')));
        await fs.rm(temp, { recursive: true, force: true, maxRetries: 30, retryDelay: 100 });
    }
}
main().catch((error) => { console.error(error.message); process.exitCode = 1; });
