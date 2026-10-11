// First-entry detection and grouped settings using the actual embedded handlers.
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { source, handler, harness, node, descendants } = require('./video_components_test_dom.cjs');
const { context, byId } = harness();
const panel = node(), content = node(), toasts = [], requests = [];
let backend;
Object.assign(context, { nativeRequest: async (channel, operation, args) => { requests.push({ operation, args }); assert.equal(operation, 'video_components_status'); return backend(); },
    refreshUpdateControls() {}, scheduleAutomaticUpdateCheck() {}, refreshImageCacheStatus() {}, refreshFrameInterpolationStatus() {}, updateSessionUi() {},
    loadHome: async () => {}, splash: node(), loginView: node(), appShell: node(), closePlayerMore() {}, showToast: (message) => toasts.push(message),
    playerPanel: panel, playerPanelContent: content, playerControls: node(), controlsTimer: 0, playerPanelTrigger: null, scheduleControlsHide() {}, positionPlayerPanel() {}, requestAnimationFrame(callback) { callback(); },
});
const defaults = source.indexOf('    const videoEnhancementDefaults = ');
vm.runInContext(source.slice(defaults, source.indexOf('    async function setFrameInterpolation(', defaults)), context);
for (const name of ['element', 'createTrackOption', 'showApp', 'renderPlayerPanel', 'presentPlayerPanel', 'openPlayerPanel', 'closePlayerPanel']) vm.runInContext(handler(name), context);
const payload = { components: ['rtx-video', 'dlssnr', 'nvidia-vfg'].map((id) => ({ id, supported: true, installed: id !== 'rtx-video', runtime: { playbackAvailable: id !== 'rtx-video' } })) };
function player() { context.player = { card: { id: 'local' }, loadInfo: {}, started: true, panelKind: '', interpolationEnabled: false, interpolationChanging: false }; panel.classList.add('hidden'); content.replaceChildren(); }
function assertGroups() {
    assert.deepEqual(descendants(content).filter((n) => n.dataset.enhancementGroup).map((n) => n.dataset.enhancementGroup), ['interpolation', 'vsr', 'nr']);
    assert.equal(descendants(content).find((n) => n.dataset.enhancementKey === 'vsr').disabled, true);
    assert.equal(descendants(content).find((n) => n.dataset.enhancementKey === 'nr').disabled, false);
    assert.equal(descendants(content).filter((n) => n.textContent === '应用设置').length, 1, 'one common apply button');
}
async function main() {
    backend = async () => payload; await context.showApp({});
    assert.equal(requests.length, 1); assert.equal(byId('player-interpolation').dataset.enhancementReady, 'true');
    player(); context.player.panelKind = 'interpolation'; context.renderPlayerPanel(); assertGroups();
    player(); vm.runInContext('videoComponentStatuses.clear()', context);
    let reply; backend = () => new Promise((resolve) => { reply = resolve; });
    const opening = context.openPlayerPanel('interpolation'); assert.equal(content.children.length, 0); reply(payload); await opening;
    assert.equal(panel.classList.contains('hidden'), false); assertGroups();
    const calls = [];
    context.setFrameInterpolation = (...args) => calls.push(args);
    const nr = content.querySelector('[data-enhancement-key="nr"]'); nr.checked = true; nr.events.change();
    assert.equal(calls.length, 0, 'editing a toggle never reloads media');
    const style = content.querySelector('[data-enhancement-key="nrStyle"]'); style.value = '2'; style.events.change();
    const apply = descendants(content).find((n) => n.textContent === '应用设置'); assert.equal(apply.disabled, false); apply.events.click();
    assert.equal(calls.length, 1); assert.equal(calls[0][1].nr, true); assert.equal(calls[0][1].nrStyle, 2);
    byId('player-interpolation').dataset.componentReady = 'true';
    const engine = content.querySelector('[data-enhancement-key="backend"]'); engine.value = 'rife'; engine.events.change();
    const models = content.querySelector('[data-enhancement-key="rifeModel"]'); assert.equal(models.children.length, 3);
    assert.equal(context.player.enhancementDraft.selection.vfg, false);
    assert.equal(calls.length, 1, 'engine choice and RIFE quality are draft edits');
    context.closePlayerPanel(); assert.equal(context.player.enhancementDraft, null);
    context.player.panelKind = 'interpolation'; context.renderPlayerPanel();
    assert.equal(content.querySelector('[data-enhancement-key="nr"]').checked, false, 'closing discards unapplied settings');
    context.player.videoEnhancement = { nr: true };
    context.player.enhancementDraft = null;
    context.updateVideoComponents({ components: [{ id: 'dlssnr', installed: true, runtime: { playbackAvailable: false, failureDetail: 'GPU 不支持' } }] });
    context.renderPlayerPanel();
    const unavailableNr = content.querySelector('[data-enhancement-key="nr"]');
    assert.equal(unavailableNr.disabled, false, 'an unavailable active component can still be disabled');
    unavailableNr.checked = false; unavailableNr.events.change();
    assert.equal(unavailableNr.disabled, true, 'unavailable component cannot be enabled again');
    player(); backend = async () => { throw new Error('native IPC unavailable'); }; await context.openPlayerPanel('interpolation');
    assert.equal(content.children.length, 0); assert.match(toasts.at(-1), /组件状态查询失败/);
    player(); backend = () => new Promise((resolve) => { reply = resolve; }); const stale = context.openPlayerPanel('interpolation'); context.player = null; reply(payload); await stale;
    assert.equal(content.children.length, 0);
    console.log('VIDEO_COMPONENT_DETECTION_OK home-first-entry/fresh-panel/group-hierarchy/draft-only/one-apply/RIFE-nested/query-error/exit-race');
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
