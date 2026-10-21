// Real embedded component status/reload handlers; no browser or server account.
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { source, html, handler, harness } = require('./video_components_test_dom.cjs');
const { context, byId, cards, timers } = harness();
let failure, response;
context.nativeRequest = async () => { if (failure) throw failure; return response; };
const status = (id, fields = {}) => ({ id, installed: false, supported: true, ...fields });
const primary = (id) => cards.get(id).children[0], secondary = (id) => cards.get(id).children[1];
context.updateVideoComponents({ components: [status('rtx-video'), status('dlssnr'), status('nvidia-vfg')] });
assert.equal(primary('rtx-video').disabled, false); assert.equal(secondary('rtx-video').disabled, true);
context.updateVideoComponents({ components: [status('rtx-video', { busy: true, progress: { phase: 'selecting' } }), status('dlssnr', { installed: true, runtime: { experimental: true, failureDetail: '推理不可用' } }), status('nvidia-vfg', { installed: true, runtime: { playbackAvailable: true } })] });
assert.match(byId('component-rtx-video-status').textContent, /请选择/); assert.equal(timers.size, 1);
assert.match(byId('component-dlssnr-status').textContent, /推理不可用/); assert.equal(byId('player-interpolation').classList.contains('hidden'), false);
assert.equal(secondary('nvidia-vfg').disabled, false, 'another component download must not block this uninstall');
assert.equal(primary('nvidia-vfg').dataset.busy, 'false', 'installed is not busy');
assert.equal(primary('nvidia-vfg').textContent, '已安装');
context.updateVideoComponents({ components: [status('rtx-video', { restartRequired: true, pendingOperation: 'install', runtime: { failureCode: 'frame_interpolation_component_invalid', failureDetail: 'payload damaged' } })] });
assert.match(byId('component-rtx-video-status').textContent, /payload damaged/); assert.equal(primary('rtx-video').disabled, true); assert.equal(secondary('rtx-video').disabled, false);
assert.equal(cards.get('rtx-video').children.length, 2, 'pending state never adds action rows');
assert.throws(() => context.updateVideoComponents({ components: [status('arbitrary-directory')] }), /身份无效/);
async function main() {
    context.updateVideoComponents({ components: [status('rtx-video', { busy: true })] }); failure = new Error('network unavailable');
    await context.refreshVideoComponents(); assert.match(byId('component-rtx-video-status').textContent, /network unavailable/); assert.equal([...timers.values()][0].delay, 3000);
    failure = undefined; response = { components: [status('rtx-video', { error: { message: '包尚未发布' } })] }; await context.refreshVideoComponents(); assert.equal(timers.size, 0); assert.match(byId('component-rtx-video-status').textContent, /包尚未发布/);
    response = { restarting: false }; await context.changeVideoComponent('rtx-video', 'restart'); assert.match(byId('component-rtx-video-status').textContent, /未确认就绪/);
    const reloadStart = source.indexOf('    const videoEnhancementDefaults = ');
    const reloadEnd = source.indexOf('    function togglePlayerFullscreen(', reloadStart);
    const requests = [], toasts = [], loadingLabels = []; let pauses = 0, plays = 0;
    Object.assign(context, {
        interpolationModels: { 'rife-v4.26': {} }, preferredInterpolationModel: 'rife-v4.26',
        playerSubtitleStyle: { fontSize: 30 }, playerPlaybackRate: 1,
        closePlayerPanel() {}, showPlayerFeedback() {}, setPlayerLoading(loading, text) { if (loading) loadingLabels.push(text); }, showPlayerControls() {},
        waitForPlayerPaint: async () => {},
        playbackPreferenceScope: () => 'series-a', playerSubtitlePosition: () => 92,
        applyPlayerPlaybackRate() {}, updateFrameInterpolationStatus() {}, showToast: (message) => toasts.push(message),
        schedulePlaybackMetadataRefresh() {},
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
    vm.runInContext(handler('interpolationLoadingLabel') + handler('playerTransitionLoadingLabel'), context);
    const label = context.videoEnhancementTransitionLabel;
    assert.equal(label(null, null, {}, { vsr: true }), '正在开启超分');
    assert.equal(label(null, null, { vsr: true }, {}), '正在关闭超分');
    assert.equal(label(null, null, {}, { nr: true }), '正在开启画面增强');
    assert.equal(label(null, null, { nr: true }, {}), '正在关闭画面增强');
    assert.match(label(null, null, {}, { vfg: true }), /正在开启 NVIDIA VFG 插帧/);
    assert.match(label(null, 'rife-v4.26', {}, {}), /正在开启 RIFE 插帧/);
    assert.equal(label('rife-v4.26', null, {}, {}), '正在关闭插帧');
    assert.equal(label(null, null, { nr: true }, { nr: true, nrTone: 0.3 }), '正在应用画面增强参数');
    assert.equal(label(null, null, { vfg: true }, { vfg: true, multiplier: 4 }), '正在应用插帧参数');
    assert.equal(label(null, null, { vsr: true }, { vsr: true, vsrQuality: 2 }), '正在应用超分参数');
    assert.equal(label('rife-v4.26', null, {}, { vfg: true }), '正在切换插帧');
    assert.equal(label(null, null, {}, { nr: true, vsr: true }), '正在应用画面设置');
    assert.equal(context.interpolationLoadingLabel({}), '正在加载视频');
    context.player = makePlayer(); await context.setFrameInterpolation(null, { vsr: true, vfg: true });
    assert.deepEqual(loadingLabels, ['正在应用画面设置', '正在应用画面设置'], 'both loading stages retain the current action after native acknowledgement');
    const firstLoad = requests.find((request) => request.operation === 'load');
    assert.equal(JSON.parse(firstLoad.args[8]).multiplier, 0, 'VFG defaults to display-aligned sampling');
    assert.deepEqual(Array.from(firstLoad.args).slice(0, 3), ['media-a', 45678, 'off']);
    assert.equal(firstLoad.args[7], 'version-b'); assert.equal(JSON.parse(firstLoad.args[8]).vsr, true); assert.equal(JSON.parse(firstLoad.args[8]).vfg, true); assert.equal(JSON.parse(firstLoad.args[8]).vsrQuality, 4);
    assert.equal(context.player.videoEnhancement.vfg, true); assert.equal(pauses, 1); assert.equal(plays, 1);
    requests.length = 0;
    context.player = makePlayer({ playing: false, videoEnhancement: { vsr: true, vfg: true } });
    await context.setFrameInterpolation('rife-v4.26', { vsr: true, vfg: true });
    const rifeLoad = requests.find((request) => request.operation === 'load');
    assert.equal(rifeLoad.args[2], '2x'); assert.equal(JSON.parse(rifeLoad.args[8]).vsr, true); assert.equal(JSON.parse(rifeLoad.args[8]).vfg, false);
    assert.equal(context.player.interpolationResumePlaying, false, 'paused state must survive the warmup reload');
    assert.equal(pauses, 1, 'switching a paused player must not send a pause command');
    assert.equal(toasts.length, 0);
    requests.length = 0;
    const parameters = { vsr: true, vfg: true, nr: true, vsrQuality: 2, vsrTarget: 2160, vfgQuality: 2, multiplier: 8, sceneDetection: false, nrStyle: 2, nrTarget: 720, nrIntensity: 0.7, nrTone: 0.6, nrStructure: 0.8, nrSkin: 1.5, nrAutoMask: true, nrUiCorrection: true };
    context.player = makePlayer({ playing: false }); await context.setFrameInterpolation(null, parameters);
    assert.deepEqual(JSON.parse(requests[0].args[8]), parameters, 'all configured values must reach the native load request');
    assert.equal(context.player.videoEnhancement.nrStyle, 2); assert.equal(context.player.interpolationResumePlaying, false);
    const oldLoad = context.player.loadInfo;
    context.player.interpolationChanging = false;
    context.nativeRequest = async () => ({ mediaSourceId: 'version-b', videoEnhancement: { ...parameters, nrTone: 1 } });
    await context.setFrameInterpolation(null, { ...parameters, nrTone: 0.3 });
    assert.equal(context.player.loadInfo, oldLoad, 'a mismatched parameter acknowledgement must not replace the active selection');
    assert.match(toasts.at(-1), /参数与请求不一致/);
    context.updateVideoComponents({ components: [status('dlssnr', { installed: true, runtime: { playbackAvailable: true, experimental: false } })] });
    assert.match(byId('component-dlssnr-status').textContent, /已安装.*原生处理/);
    console.log('VIDEO_COMPONENT_UI_TEST_OK independent-status/damaged-pending/retry/restart-failure/native-reload-progress-version/RIFE-VFG-exclusion/NR-ready/all-provider-parameters/parameter-mismatch');
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
