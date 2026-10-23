// Isolated real CEF playback probe. The app must use a private config/cache and
// local VFG mpv.conf; legacy playerLoad refuses a configured account session.
// node test_vfg_parameter_menu.cjs <CDP port> <local clip> <result directory>
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');
const port = Number(process.argv[2]), clip = path.resolve(process.argv[3]), output = path.resolve(process.argv[4]);
assert.ok(port > 0 && port < 65536 && fs.statSync(clip).isFile());
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
async function main() {
    fs.mkdirSync(output, { recursive: true });
    const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    const page = browser.contexts()[0].pages().find(page => page.url() === 'app://resources/mediastation.html');
    assert.ok(page);
    const session = await page.context().newCDPSession(page), scripts = [];
    session.on('Debugger.scriptParsed', script => scripts.push(script));
    await session.send('Debugger.enable');
    const script = scripts.find(script => script.url === 'app://resources/mediastation.js');
    assert.ok(script);
    assert.equal((await session.send('Debugger.getScriptSource', { scriptId: script.scriptId })).scriptSource, source, 'embedded source matches current source');
    const lineNumber = source.slice(0, source.indexOf('        const panelTrigger =', source.indexOf('    async function openPlayerPanel('))).split('\n').length - 1;
    const { breakpointId } = await session.send('Debugger.setBreakpoint', { location: { scriptId: script.scriptId, lineNumber } });
    const paused = new Promise(resolve => session.once('Debugger.paused', resolve));
    const trigger = page.evaluate(() => { const button = document.getElementById('player-info'); button.disabled = false; button.click(); });
    try {
        const pause = await paused;
        const exported = await session.send('Debugger.evaluateOnCallFrame', { callFrameId: pause.callFrames[0].callFrameId, expression: `window.__vfgMenuProbe={
            setup(){
                player={card:{id:'local-vfg-menu-probe',type:'Movie'},started:true,playing:true,loadInfo:{audioTracks:[],subtitleTracks:[]},videoEnhancement:videoEnhancementSelection({vfg:true}),panelKind:'',positionMs:0};
                updateVideoComponents({components:['nvidia-vfg'].map(id=>({id,installed:true,supported:true,runtime:{playbackAvailable:true}}))});
                splash.classList.add('hidden');loginView.classList.add('hidden');appShell.classList.add('hidden');playerView.classList.remove('hidden');
                setPlayerMode(true);setPlayerLoading(false);showPlayerControls();presentPlayerPanel(player,'interpolation',byId('player-interpolation'));
            },
            diagnostic:()=>nativeRequest('mediaStationFrameInterpolation','frame_interpolation_diagnostics',['frame_interpolation_diagnostics','local-vfg-menu-probe'],15000)
        }` });
        assert.ok(!exported.exceptionDetails);
        await session.send('Debugger.removeBreakpoint', { breakpointId });
        await session.send('Debugger.resume'); await trigger;
        await page.evaluate(async clip => {
            window.__menuProbe = { events: [], playback: [], firstPosition: 0 };
            const original = window._onMediaStationResponse;
            window._onMediaStationResponse = (id, op, ok, json) => {
                if (op !== 'playback_event') return original(id, op, ok, json);
                const event = JSON.parse(json); window.__menuProbe.playback.push(event.kind);
                if (event.kind === 'position') window.__menuProbe.firstPosition = event.positionMs;
            };
            window.jmpNative.playerLoad(clip,0,1,1,0,JSON.stringify({Id:'local-vfg-menu-probe',Name:'Local VFG menu probe',Type:'Movie'}));
            await new Promise(resolve => setTimeout(resolve,500)); window.jmpNative.playerPlay();
        }, clip);
        await page.waitForFunction(() => window.__menuProbe.firstPosition > 1000, null, { timeout: 15000 });
        await page.waitForTimeout(3000);
        await page.evaluate(() => window.__vfgMenuProbe.setup());
        const diagnostic = await page.evaluate(() => window.__vfgMenuProbe.diagnostic());
        assert.ok(diagnostic.playback.vfg?.outputFrames > 0, 'real VFG outputs required');
        const results = [];
        for (const fullscreen of [false, true]) {
            if (fullscreen) { await page.evaluate(() => window.jmpNative.toggleFullscreen()); await page.waitForTimeout(700); }
            await page.evaluate(() => {
                const control = document.querySelector('[data-enhancement-key="multiplier"]');
                const menu = document.getElementById('player-enhancement-multiplier-options');
                window.__menuProbe.events = [];
                control.onpointerdown = () => { window.__menuProbe.down = performance.now(); };
                menu.addEventListener('beforetoggle', event => {
                    if (event.newState !== 'open') return;
                    const row = { unixMs:Date.now(),handlerMs:performance.now()-window.__menuProbe.down };
                    requestAnimationFrame(() => { row.firstRafMs=performance.now()-window.__menuProbe.down; });
                    window.__menuProbe.events.push(row);
                }, { signal:(window.__menuProbe.listenerController ||= new AbortController()).signal });
            });
            const control = page.locator('[data-enhancement-key="multiplier"]');
            for (let i = 0; i < 20; i++) {
                // Use actual CEF mouse input, including its native UI dispatch.
                await control.click();
                await page.locator('#player-enhancement-multiplier-options:popover-open').waitFor();
                await page.waitForTimeout(30);
                await page.keyboard.press('Escape');
            }
            await control.click();
            await page.screenshot({ path:path.join(output,fullscreen?'fullscreen-menu.png':'window-menu.png') });
            await page.keyboard.press('Escape');
            const events = await page.evaluate(() => { const rows=window.__menuProbe.events;window.__menuProbe.listenerController.abort();window.__menuProbe.listenerController=null;return rows; });
            assert.equal(events.length,21);
            assert.ok(events.every(row => row.firstRafMs >= 0 && row.firstRafMs < 100), 'menu should reach next CEF frame within 100 ms');
            results.push({ fullscreen,events });
        }
        const final = await page.evaluate(async () => ({diagnostic:await window.__vfgMenuProbe.diagnostic(),playback:window.__menuProbe.playback}));
        assert.ok(!final.playback.includes('error'));
        assert.ok(final.diagnostic.playback.vfg.outputFrames > diagnostic.playback.vfg.outputFrames, 'VFG keeps producing throughout UI probe');
        fs.writeFileSync(path.join(output,'result.json'),JSON.stringify({port,diagnostic,final,results,presentation:'real CEF mouse input and menu frames, not optical scanout'},null,2));
        console.log(`VFG_PARAMETER_MENU_OK target=${diagnostic.playback.vfg.targetFps} opens=42 max-first-raf-ms=${Math.max(...results.flatMap(row=>row.events.map(event=>event.firstRafMs)))}`);
    } finally {
        await session.send('Debugger.resume').catch(() => {});
        await page.evaluate(() => window.jmpNative.playerStop());
        await session.detach(); await browser.close();
    }
}
main().catch(error => { console.error(error); process.exitCode=1; });
