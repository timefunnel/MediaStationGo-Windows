// Real playback markup and event handlers, with explicit native IPC responses.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');
const root = path.resolve(__dirname, '../..');
const web = path.join(root, 'src/web');
const output = path.join(root, 'build/player-controls-ui');
const source = fs.readFileSync(path.join(web, 'mediastation.js'), 'utf8');
const hook = `
    const calls = [], requests = [];
    const components = {components:['rtx-video','dlssnr','nvidia-vfg'].map(id=>({id,installed:true,supported:true,runtime:{playbackAvailable:true}}))};
    const rifeStatus = {component:{installed:true,supported:true},componentStatus:'ready',models:[{},{},{}]};
    const tracks = {mediaSourceId:'version-b',audioTracks:[{id:1,title:'中文'}],subtitleTracks:[]};
    window.jmpNative = Object.fromEntries(['playerPause','playerPlay','playerStop','playerSeek','toggleFullscreen','playerSetVolume','playerSetMuted','playerSetSpeed','playerSetSubtitleStyle','playerSetDanmaku','playerSetDanmakuStyle','themeColor'].map(name=>[name,(...args)=>calls.push({name,args})]));
    let loadOrder = 'after', loadFailure = '', releaseLoad;
    nativeRequest = async (channel, operation, args) => {
        requests.push({operation,args});
        if(operation==='video_components_status') return components;
        if(operation==='frame_interpolation_set_model') return rifeStatus;
        if(operation==='tracks') return tracks;
        if(operation!=='load') throw new Error('Unexpected fixture IPC: '+operation);
        if(loadOrder==='hold') await new Promise(resolve=>{releaseLoad=resolve;});
        if(loadFailure==='reject') throw new Error('test native load rejected');
        if(loadOrder==='before') handlePlaybackEvent({kind:'started'});
        const selection=JSON.parse(args[8]);
        if(loadFailure==='mismatch') selection.nrTone=0.17;
        return {...tracks,videoEnhancement:selection,frameInterpolation:args[2]==='2x'?{modelId:args[3]}:null};
    };
    function reset() {
        closeDanmakuPanel(); closePlayerPanel(false); hidePlayerFeedback();
        window.clearTimeout(toastTimer); toast.classList.add('hidden');
        window.clearTimeout(playerClickTimer); playerClickTimer=0; playerClickAt=0;
        player={card:{id:'movie-a',title:'测试影片',type:'Movie',playable:true},started:true,playing:true,positionMs:45678,durationMs:120000,
            loadInfo:{...tracks,videoEnhancement:videoEnhancementSelection()},videoEnhancement:videoEnhancementSelection(),
            interpolationEnabled:false,interpolationModel:null,panelKind:'',buffering:false};
        loadOrder='after'; loadFailure=''; calls.length=0; requests.length=0;
        splash.classList.add('hidden'); loginView.classList.add('hidden'); appShell.classList.add('hidden');
        playerView.classList.remove('hidden'); setPlayerMode(true); setPlayerLoading(false);
        updateVideoComponents(components); updateFrameInterpolationStatus(rifeStatus);
        refreshPlayerTools(); updatePlayerProgress(); showPlayerControls();
    }
    window.fixture={calls,requests,reset,startPlayback,setFrameInterpolation,handlePlaybackEvent,closePlayerPanel,loadVideoEnhancementSettings,
        selection:videoEnhancementSelection,storageKey:videoEnhancementSettingKey,
        get player(){return player;}, set order(value){loadOrder=value;},set failure(value){loadFailure=value;},release(){releaseLoad();},
        discardPlayer(){player=null;},showPlayerControls};
    reset(); document.documentElement.dataset.ready='true';
`;
assert.equal(source.split('    initialize();').length, 2, 'one initialization boundary');
const script = source.replace('    initialize();', hook);
async function choose(page, key, value) {
    await page.locator(`[data-enhancement-key="${key}"]`).click();
    await page.locator(`#player-enhancement-${key}-options [role="option"][value="${value}"]`).click();
}
async function main() {
    fs.mkdirSync(output, { recursive: true });
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    await page.route('http://player.test/**', async route => {
        const file = new URL(route.request().url()).pathname.split('/').at(-1) || 'mediastation.html';
        const body = file === 'mediastation.js' ? script : fs.readFileSync(path.join(web, file));
        await route.fulfill({ body, contentType: file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html' });
    });
    const nativeCount = name => page.evaluate(name => fixture.calls.filter(c => c.name === name).length, name);
    const loadCount = () => page.evaluate(() => fixture.requests.filter(r => r.operation === 'load').length);
    const saved = () => page.evaluate(() => localStorage.getItem(fixture.storageKey));
    const reset = () => page.evaluate(() => fixture.reset());
    try {
        await page.goto('http://player.test/mediastation.html');
        await page.waitForSelector('html[data-ready="true"]');
        const bottom = await page.locator('.player-bottom').boundingBox();
        await page.mouse.click(640, bottom.y + 15);
        await page.mouse.dblclick(640, bottom.y + 15);
        await page.waitForTimeout(470);
        assert.equal(await nativeCount('playerPause'), 0, 'bottom padding never pauses');
        assert.equal(await nativeCount('toggleFullscreen'), 0, 'bottom padding never enters fullscreen');
        await page.locator('#player-playback').click();
        await page.waitForTimeout(470);
        assert.equal(await nativeCount('playerPause'), 1, 'playback button acts exactly once');
        await reset(); await page.mouse.click(640, 350); await page.waitForTimeout(470);
        assert.equal(await nativeCount('playerPause'), 1, 'stage single click pauses');
        await reset(); await page.mouse.dblclick(640, 350); await page.waitForTimeout(470);
        assert.equal(await nativeCount('playerPause'), 0, 'stage double click does not also pause');
        assert.equal(await nativeCount('toggleFullscreen'), 1);
        await reset(); await page.mouse.click(640, 350); await page.mouse.click(640, bottom.y + 15); await page.waitForTimeout(470);
        assert.equal(await nativeCount('playerPause'), 0, 'entering controls cancels a pending stage click');
        assert.equal(await page.locator('#player-more-menu button').count(), 1);
        for (const id of ['player-danmaku', 'player-danmaku-settings', 'player-interpolation']) assert.equal(await page.locator('#' + id).isVisible(), true);

        const popup = page.locator('#player-danmaku-panel');
        await page.locator('#player-danmaku-settings').dispatchEvent('click');
        assert.equal(await popup.isVisible(), false, 'click alone cannot open or pin settings');
        await page.locator('#player-danmaku').hover(); await page.waitForTimeout(240);
        assert.equal(await popup.isVisible(), false, 'toggle hover does not open settings');
        await page.locator('#player-danmaku-settings').hover(); await page.waitForTimeout(240);
        assert.equal(await popup.isVisible(), true);
        await popup.hover(); await page.waitForTimeout(260);
        assert.equal(await popup.isVisible(), true, 'pointer can cross into the panel');
        assert.equal(await popup.locator('input[type=range]').count(), 5);
        await page.locator('#player-danmaku-fixed-seconds').focus();
        await page.keyboard.press('ArrowRight');
        assert.equal(await page.evaluate(() => fixture.player.positionMs), 45678, 'range arrows never seek the video');
        await page.locator('#player-danmaku-settings').click(); await page.mouse.move(640, 350); await page.waitForTimeout(280);
        assert.equal(await popup.isVisible(), false, 'click/focus never keeps hover settings pinned');
        assert.equal(await nativeCount('playerPause'), 0);

        await page.locator('#player-interpolation').click();
        const apply = page.getByRole('button', { name: '应用', exact: true });
        await apply.waitFor(); assert.equal(await apply.isDisabled(), true);
        await page.locator('[data-enhancement-key=backend][value=nvidia-vfg]').check();
        assert.equal(await page.locator('.enhancement-settings select').count(), 0, 'enhancement choices never enter the native select popup path');
        const multiplier = page.locator('[data-enhancement-key=multiplier]');
        assert.equal(await multiplier.textContent(), '刷新率自适应');
        assert.deepEqual(await page.locator('#player-enhancement-multiplier-options [role=option]').allTextContents(), ['刷新率自适应', '2 倍', '3 倍']);
        await multiplier.focus(); await page.keyboard.press('ArrowDown');
        assert.equal(await multiplier.getAttribute('aria-expanded'), 'true');
        await page.keyboard.press('End'); await page.keyboard.press('Enter');
        assert.equal(await multiplier.textContent(), '3 倍');
        await multiplier.click(); await page.keyboard.press('Escape');
        assert.equal(await multiplier.getAttribute('aria-expanded'), 'false');
        assert.equal(await page.locator('#player-panel').isVisible(), true, 'first Escape closes the choices only');
        assert.equal(await multiplier.evaluate(node => document.activeElement === node), true);
        await multiplier.click(); await page.locator('#player-panel-title').click();
        assert.equal(await multiplier.getAttribute('aria-expanded'), 'false', 'outside click dismisses choices');
        await choose(page, 'multiplier', '3');
        await page.locator('[data-enhancement-key=nr]').check();
        await page.locator('[data-enhancement-group=nr] summary').click();
        await choose(page, 'nrStyle', '2');
        assert.equal(await loadCount(), 0, 'all edits stay in the draft');
        assert.equal(await saved(), null);
        await apply.click();
        await page.waitForFunction(() => fixture.player.enhancementLoadConfirmed);
        assert.equal(await loadCount(), 1, 'one apply makes one reload');
        assert.equal(await saved(), null, 'load acknowledgement alone is insufficient to persist');
        assert.equal(await page.locator('#player-panel').isVisible(), true, 'panel stays until playback is confirmed');
        await page.evaluate(() => fixture.handlePlaybackEvent({kind:'started'}));
        let setting = JSON.parse(await saved());
        assert.equal(setting.selection.multiplier, 3); assert.equal(setting.selection.nrStyle, 2); assert.equal(setting.selection.nr, true);
        assert.equal(await page.locator('#player-panel').isVisible(), false, 'successful reload closes the panel');
        await page.locator('#player-interpolation').click();
        assert.equal(await apply.isDisabled(), true);
        await page.locator('[data-enhancement-key=backend][value=off]').check();
        await page.locator('[data-enhancement-key=nr]').uncheck();
        await page.locator('#player-panel-close').click();
        await page.locator('#player-interpolation').click();
        assert.equal(await page.locator('[data-enhancement-key=backend][value=nvidia-vfg]').isChecked(), true, 'close discards unapplied edits');
        assert.equal(await page.locator('[data-enhancement-key=nr]').isChecked(), true);
        await page.locator('[data-enhancement-key=nr]').uncheck();
        await page.locator('#player-speed').click();
        await page.locator('#player-interpolation').click();
        assert.equal(await page.locator('[data-enhancement-key=nr]').isChecked(), true, 'switching to another panel discards the draft');

        await page.locator('[data-enhancement-key=backend][value=rife]').check();
        await choose(page, 'rifeModel', 'rife-v4.26-scale0.5');
        await page.evaluate(() => { fixture.order='before'; });
        await apply.click();
        await page.waitForFunction(() => fixture.player.interpolationChanging === false);
        setting = JSON.parse(await saved());
        assert.equal(setting.model, 'rife-v4.26-scale0.5'); assert.equal(setting.selection.vfg, false);
        assert.equal(setting.selection.multiplier, 3, 'disabled VFG retains its parameters');
        assert.equal(await page.locator('#player-panel').isVisible(), false, 'playback-before-ack order also closes the panel');
        await page.evaluate(() => fixture.startPlayback({id:'movie-b',type:'Movie',title:'下一部影片',playable:true},12345,'version-b'));
        const start = await page.evaluate(() => fixture.requests.filter(r => r.operation === 'load').at(-1).args);
        assert.equal(start[0], 'movie-b'); assert.equal(start[2], '2x'); assert.equal(start[3], setting.model); assert.equal(start[7], 'version-b');
        assert.deepEqual(JSON.parse(start[8]), setting.selection, 'next playback uses the full accepted configuration');
        await page.evaluate(async () => { await fixture.setFrameInterpolation(null,{...fixture.player.videoEnhancement,vsr:false,vfg:false,nr:false}); });
        const disabled = JSON.parse(await saved());
        assert.equal(disabled.model, null); assert.equal(disabled.selection.nr, false);
        assert.equal(disabled.selection.nrStyle, 2); assert.equal(disabled.selection.multiplier, 3, 'turning enhancements off preserves their parameters');

        await reset(); const beforeStorageFailure = await saved();
        await page.evaluate(async () => {
            const setItem=Storage.prototype.setItem;
            Storage.prototype.setItem=function(key,value){if(key===fixture.storageKey)throw new Error('test storage denied');return setItem.call(this,key,value);};
            try {
                fixture.order='before'; await fixture.setFrameInterpolation(null,fixture.selection({vsr:true}));
            } finally { Storage.prototype.setItem=setItem; }
        });
        assert.equal(await saved(), beforeStorageFailure);
        assert.equal(await page.evaluate(() => fixture.player.videoEnhancement.vsr), true, 'storage failure does not undo accepted native settings');
        assert.match(await page.locator('#toast').textContent(), /设置保存失败.*test storage denied/);

        for (const failure of ['reject', 'mismatch']) {
            await reset(); const previous = await saved();
            await page.locator('#player-interpolation').click();
            await page.evaluate(async failure => { fixture.failure=failure; await fixture.setFrameInterpolation(null,fixture.selection({vsr:true})); }, failure);
            assert.equal(await saved(), previous, 'failed application never replaces the last good settings');
            assert.equal(await page.evaluate(() => fixture.player.interpolationChanging), false);
            assert.equal(await page.locator('#player-panel').isVisible(), true, 'failed reload preserves the panel');
            assert.match(await page.locator('#toast').textContent(), failure === 'reject' ? /native load rejected/ : /参数与请求不一致/);
        }
        await reset();
        await page.evaluate(() => { fixture.order='hold'; window.applying=fixture.setFrameInterpolation(null,fixture.selection({vsr:true})); });
        await page.waitForFunction(() => fixture.requests.some(r => r.operation === 'load'));
        const beforeExit = await saved();
        await page.evaluate(async () => { fixture.discardPlayer(); fixture.release(); await applying; });
        assert.equal(await saved(), beforeExit, 'a late response after exit cannot save settings');

        await page.evaluate(() => localStorage.setItem(fixture.storageKey, JSON.stringify({model:null,selection:fixture.selection({vfg:true,multiplier:8,nrTone:0.4})})));
        const migrated = await page.evaluate(() => fixture.loadVideoEnhancementSettings());
        assert.equal(migrated.selection.multiplier, 3);
        assert.equal(migrated.selection.nrTone, 0.4, 'migration preserves other parameters');
        assert.deepEqual(JSON.parse(await saved()), migrated);
        assert.match(await page.locator('#toast').textContent(), /原 8 倍设置调整为 3 倍/);

        for (const theme of ['dark', 'light']) {
            await reset();
            await page.evaluate(theme => { document.documentElement.dataset.uiTheme=theme; }, theme);
            for (const width of [1280, 375]) {
                await page.setViewportSize({width,height:800});
                await page.evaluate(() => fixture.showPlayerControls());
                await page.locator('#player-danmaku-settings').hover(); await page.waitForTimeout(240);
                let rect = await popup.boundingBox(); assert.ok(rect.x >= 0 && rect.x + rect.width <= width, 'danmaku panel fits viewport');
                await page.screenshot({path:path.join(output,`danmaku-${theme}-${width}.png`)});
                await page.mouse.move(80,300); await page.waitForTimeout(280);
                await page.locator('#player-interpolation').click();
                await apply.waitFor();
                await page.locator('[data-enhancement-key=backend][value=nvidia-vfg]').check();
                await multiplier.click();
                const menu = page.locator('#player-enhancement-multiplier-options');
                const menuRect = await menu.boundingBox();
                assert.ok(menuRect.x >= 0 && menuRect.x + menuRect.width <= width && menuRect.y >= 0 && menuRect.y + menuRect.height <= 800, 'choice menu fits viewport');
                await page.screenshot({path:path.join(output,`enhancement-choices-${theme}-${width}.png`)});
                await page.keyboard.press('Escape');
                rect = await page.locator('#player-panel').boundingBox();
                assert.ok(rect.x >= 0 && rect.x + rect.width <= width && rect.y >= 0, 'enhancement panel fits viewport');
                await page.screenshot({path:path.join(output,`enhancement-${theme}-${width}.png`)});
                await page.locator('#player-panel-close').click();
                assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
            }
            await page.setViewportSize({width:1280,height:800});
        }
        assert.deepEqual(errors, [], 'browser errors');
        console.log('PLAYER_CONTROLS_BROWSER_OK bottom/stage-clicks/hover-only/range-keyboard/draft/apply/persistence/both-callback-orders/next-playback/failure/exit-race/light/dark/narrow');
    } finally { await browser.close(); }
}
main().catch(error => { console.error(error); process.exitCode=1; });
