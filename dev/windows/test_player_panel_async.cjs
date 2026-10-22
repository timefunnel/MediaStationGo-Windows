// Exercise delayed native replies against the actual panel handlers.
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { handler, node } = require('./video_components_test_dom.cjs');
const panel = node('div', 'hidden');
let pending = [], applied = [], renders = 0;
const context = vm.createContext({
    player: { card: { id: 'media' }, loadInfo: {} }, playerPanel: panel,
    playerPanelTrigger: null,
    closePlayerMore() {}, closeDanmakuPanel() {}, refreshPlayerTools() {},
    scheduleControlsHide() {}, positionPlayerPanel() {},
    renderPlayerPanel() { renders++; },
    presentPlayerPanel(player, kind, trigger) {
        player.panelKind = kind; context.playerPanelTrigger = trigger;
        panel.classList.remove('hidden'); renders++;
    },
    nativeRequest: () => new Promise((resolve, reject) => pending.push({ resolve, reject })),
    refreshVideoComponents: () => new Promise(resolve => pending.push({ resolve })),
    applyPlaybackMetadataRefresh(player, tracks) { applied.push(tracks); },
    updateFrameInterpolationStatus() {}, friendlyError: error => error.message,
});
vm.runInContext(handler('openPlayerPanel') + handler('closePlayerPanel'), context);
async function main() {
    const first = context.openPlayerPanel('audio');
    assert.equal(panel.classList.contains('hidden'), false, 'panel opens before native reply');
    assert.equal(context.player.panelLoading, true);
    context.closePlayerPanel(false);
    pending.shift().resolve({ stale: true }); await first;
    assert.equal(panel.classList.contains('hidden'), true, 'late reply cannot reopen closed panel');
    assert.equal(applied.length, 0);
    const old = context.openPlayerPanel('audio');
    await context.openPlayerPanel('speed');
    pending.shift().resolve({ stale: true }); await old;
    assert.equal(context.player.panelKind, 'speed', 'late reply cannot replace a newer panel');
    assert.equal(applied.length, 0);
    const failure = context.openPlayerPanel('info');
    pending.shift().reject(new Error('native query failed')); await failure;
    assert.equal(context.player.panelError, 'native query failed');
    assert.equal(context.player.panelLoading, false);
    context.closePlayerPanel(false);
    const success = context.openPlayerPanel('audio');
    pending.shift().resolve({ current: true }); await success;
    assert.equal(applied.length, 1); assert.equal(context.player.panelError, '');
    context.closePlayerPanel(false);
    const a = context.openPlayerPanel('audio'); context.closePlayerPanel(false);
    const b = context.openPlayerPanel('audio');
    pending.shift().resolve({ stale: true }); await a;
    assert.equal(context.player.panelLoading, true, 'older same-kind reply cannot end newer loading state');
    pending.shift().resolve({ latest: true }); await b;
    assert.equal(applied.length, 2);
    context.closePlayerPanel(false);
    const component = context.openPlayerPanel('interpolation');
    pending.shift().resolve(false); await component;
    assert.match(context.player.panelError, /组件状态查询失败/);
    assert.ok(renders > 0);
    console.log('PLAYER_PANEL_ASYNC_OK immediate/close/switch/same-kind/failure/component-failure');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
