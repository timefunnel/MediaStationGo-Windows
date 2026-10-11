// Real embedded component status/reload handlers; no browser or server account.
const assert = require('node:assert/strict');
const fs = require('node:fs'); const vm = require('node:vm'); const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.html'), 'utf8');
assert.ok(html.indexOf('id="components-page"') < html.indexOf('id="settings-install-frame-interpolation"'));
assert.ok(!html.slice(html.indexOf('data-settings-section="playback">'), html.indexOf('data-settings-section="storage">')).includes('id="settings-install-frame-interpolation"'));
function node() { const classes = new Set(); return { dataset: {}, disabled: false, textContent: '', classList: { add: (v) => classes.add(v), remove: (v) => classes.delete(v), contains: (v) => classes.has(v), toggle(v, on) { on ? classes.add(v) : classes.delete(v); } } }; }
const elements = new Map(); function byId(id) { assert.ok(html.includes(`id="${id}"`), id); if (!elements.has(id)) elements.set(id, node()); return elements.get(id); }
const cards = new Map();
for (const id of ['rtx-video', 'dlssnr', 'nvidia-vfg']) {
    const buttons = ['install', 'import', 'uninstall', 'restart', 'cancel'].map((operation) => { const button = node(); button.dataset.componentOperation = operation; return button; });
    const pending = node(); cards.set(id, { buttons, pending, querySelectorAll() { return buttons; }, querySelector() { return pending; } });
}
const timers = new Map(); let timerId = 0; let failure; let response;
const context = vm.createContext({ byId, document: { querySelector(selector) { return cards.get(selector.match(/"([^"]+)"/)[1]); } },
    window: { clearTimeout(id) { timers.delete(id); }, setTimeout(fn, delay) { const id = ++timerId; timers.set(id, { fn, delay }); return id; } },
    nativeRequest: async () => { if (failure) throw failure; return response; }, formatBytes: (v) => `${v} B`, friendlyError: (error) => error.message, refreshPlayerTools() {} });
const start = source.indexOf('    const videoComponentStatuses = new Map();'); const end = source.indexOf('    function openSettings(', start);
vm.runInContext(source.slice(start, end), context);
const status = (id, fields = {}) => ({ id, installed: false, supported: true, ...fields });
context.updateVideoComponents({ components: [status('rtx-video'), status('dlssnr'), status('nvidia-vfg')] });
assert.equal(cards.get('rtx-video').buttons[0].disabled, false); assert.equal(cards.get('rtx-video').buttons[2].disabled, true);
context.updateVideoComponents({ components: [status('rtx-video', { busy: true, progress: { phase: 'selecting' } }), status('dlssnr', { installed: true, runtime: { experimental: true, failureDetail: '推理不可用' } }), status('nvidia-vfg', { installed: true, runtime: { playbackAvailable: true } })] });
assert.match(byId('component-rtx-video-status').textContent, /请选择/); assert.equal(timers.size, 1);
assert.match(byId('component-dlssnr-status').textContent, /推理不可用/); assert.equal(byId('player-interpolation').classList.contains('hidden'), false);
assert.equal(cards.get('nvidia-vfg').buttons[2].disabled, false, 'another component download must not block this uninstall');
context.updateVideoComponents({ components: [status('rtx-video', { restartRequired: true, pendingOperation: 'install', runtime: { failureCode: 'frame_interpolation_component_invalid', failureDetail: 'payload damaged' } })] });
assert.match(byId('component-rtx-video-status').textContent, /payload damaged/); assert.equal(cards.get('rtx-video').buttons[3].disabled, true); assert.equal(cards.get('rtx-video').buttons[4].disabled, false);
assert.throws(() => context.updateVideoComponents({ components: [status('arbitrary-directory')] }), /身份无效/);
async function main() {
    context.updateVideoComponents({ components: [status('rtx-video', { busy: true })] }); failure = new Error('network unavailable');
    await context.refreshVideoComponents(); assert.match(byId('component-rtx-video-status').textContent, /network unavailable/); assert.equal([...timers.values()][0].delay, 3000);
    failure = undefined; response = { components: [status('rtx-video', { error: { message: '包尚未发布' } })] }; await context.refreshVideoComponents(); assert.equal(timers.size, 0); assert.match(byId('component-rtx-video-status').textContent, /包尚未发布/);
    response = { restarting: false }; await context.changeVideoComponent('rtx-video', 'restart'); assert.match(byId('component-rtx-video-status').textContent, /未确认就绪/);
    const reloadStart = source.indexOf('    async function setFrameInterpolation(');
    const reloadEnd = source.indexOf('    function togglePlayerFullscreen(', reloadStart);
    const requests = [], toasts = []; let pauses = 0, plays = 0;
    Object.assign(context, {
        interpolationModels: { 'rife-v4.26': {} }, preferredInterpolationModel: 'rife-v4.26',
        playerSubtitleStyle: { fontSize: 30 }, playerPlaybackRate: 1,
        closePlayerPanel() {}, showPlayerFeedback() {}, setPlayerLoading() {}, showPlayerControls() {},
        playerTransitionLoadingLabel: () => '正在加载', waitForPlayerPaint: async () => {},
        playbackPreferenceScope: () => 'series-a', playerSubtitlePosition: () => 92,
        applyPlayerPlaybackRate() {}, updateFrameInterpolationStatus() {}, showToast: (message) => toasts.push(message),
        nativeRequest: async (channel, operation, args) => {
            requests.push({ channel, operation, args });
            if (operation === 'frame_interpolation_set_model') return {};
            const selection = JSON.parse(args[8]);
            return { mediaSourceId: args[7], frameInterpolation: { modelId: args[2] === '2x' ? args[3] : null }, videoEnhancement: selection };
        },
    });
    context.window.jmpNative = { playerPause() { pauses++; }, playerPlay() { plays++; } };
    const makePlayer = (fields = {}) => ({ started: true, playing: true, card: { id: 'media-a' }, positionMs: 45678,
        loadInfo: { mediaSourceId: 'version-b' }, interpolationEnabled: false, videoEnhancement: { vsr: false, vfg: false }, ...fields });
    vm.runInContext(source.slice(reloadStart, reloadEnd), context);
    context.player = makePlayer(); await context.setFrameInterpolation(null, { vsr: true, vfg: true });
    const firstLoad = requests.find((request) => request.operation === 'load');
    assert.deepEqual(Array.from(firstLoad.args).slice(0, 3), ['media-a', 45678, 'off']);
    assert.equal(firstLoad.args[7], 'version-b'); assert.deepEqual(JSON.parse(firstLoad.args[8]), { vsr: true, vfg: true });
    assert.equal(context.player.videoEnhancement.vfg, true); assert.equal(pauses, 1); assert.equal(plays, 1);
    requests.length = 0;
    context.player = makePlayer({ playing: false, videoEnhancement: { vsr: true, vfg: true } });
    await context.setFrameInterpolation('rife-v4.26', { vsr: true, vfg: true });
    const rifeLoad = requests.find((request) => request.operation === 'load');
    assert.equal(rifeLoad.args[2], '2x'); assert.deepEqual(JSON.parse(rifeLoad.args[8]), { vsr: true, vfg: false });
    assert.equal(context.player.interpolationResumePlaying, false, 'paused state must survive the warmup reload');
    assert.equal(pauses, 1, 'switching a paused player must not send a pause command');
    assert.equal(toasts.length, 0);
    console.log('VIDEO_COMPONENT_UI_TEST_OK independent-status/damaged-pending/experimental-NR/retry/restart-failure/native-reload-progress-version/RIFE-VFG-exclusion');
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
