// Exercises the actual embedded settings functions, without a browser or account.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.html'), 'utf8');
const elements = new Map();
const element = (id) => {
    assert.ok(html.includes(`id="${id}"`), `missing HTML element ${id}`);
    if (!elements.has(id)) {
        const classes = new Set();
        elements.set(id, { textContent: '', disabled: false, dataset: {}, classList: {
            add(name) { classes.add(name); },
            remove(name) { classes.delete(name); },
            toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); },
            contains(name) { return classes.has(name); },
        }, focus() { this.focused = true; } });
    }
    return elements.get(id);
};
const timers = new Map();
let nextTimer = 0;
let response;
let requestFailure;
let requests = [];
const context = vm.createContext({
    byId: element, interpolationModels: {}, interpolationComponentTimer: 0, interpolationComponentTracking: false,
    interpolationRestartPrompted: false, interpolationComponentRestarting: false,
    window: {
        clearTimeout(id) { timers.delete(id); },
        setTimeout(fn, delay) { const id = ++nextTimer; timers.set(id, { fn, delay }); return id; },
    },
    formatBytes: (bytes) => `${bytes} B`, friendlyError: (error) => error.message,
    nativeRequest: async (...args) => { requests.push(args); if (requestFailure) throw requestFailure; return response; },
});
const start = source.indexOf('    function updateFrameInterpolationStatus(');
const end = source.indexOf('    function scheduleImageCacheStatsRefresh()', start);
assert.ok(start >= 0 && end > start);
vm.runInContext(source.substring(start, end), context);
const label = element('settings-frame-interpolation-status');
const install = element('settings-install-frame-interpolation');
const uninstall = element('settings-uninstall-frame-interpolation');
const cancelInstall = element('settings-cancel-install-frame-interpolation');
const pendingInstall = element('settings-pending-frame-interpolation-install');
const restart = element('settings-restart-frame-interpolation');
const confirmRestart = element('settings-confirm-restart-frame-interpolation');
const restartConfirmation = element('settings-confirm-frame-interpolation-restart');
assert.match(html, /id="settings-install-frame-interpolation" class="primary-command"/);
function render(component, fields = {}) {
    context.updateFrameInterpolationStatus({ componentStatus: 'unavailable', component, ...fields });
}
render({ installed: false });
assert.equal(install.disabled, false);
assert.equal(uninstall.disabled, true);
assert.equal(cancelInstall.disabled, true);
assert.equal(pendingInstall.classList.contains('hidden'), true);
assert.match(label.textContent, /普通播放不受影响/);
render({ installed: false, busy: true, progress: { phase: 'downloading', downloadedBytes: 5, totalBytes: 9, source: 'GitHub 直连' } });
assert.equal(install.disabled, true);
assert.equal(uninstall.disabled, true);
assert.match(label.textContent, /5 B \/ 9 B · GitHub 直连/);
assert.equal(timers.size, 1);
render({ installed: false, restartRequired: true, pendingOperation: 'install' });
assert.equal(install.disabled, true);
assert.match(label.textContent, /重新启动后生效/);
assert.equal(timers.size, 0);
assert.equal(cancelInstall.disabled, false);
assert.equal(pendingInstall.classList.contains('hidden'), false);
assert.equal(restartConfirmation.classList.contains('hidden'), false);
assert.equal(confirmRestart.focused, true);
assert.equal(requests.length, 0, 'installation completion never restarts without confirmation');
context.deferFrameInterpolationRestart();
render({ installed: false, restartRequired: true, pendingOperation: 'install' });
assert.equal(restartConfirmation.classList.contains('hidden'), true, 'later remains respected on repeated status queries');
context.showFrameInterpolationRestartConfirmation();
assert.equal(restartConfirmation.classList.contains('hidden'), false);
render({ installed: false, restartRequired: true, pendingOperation: 'install' }, { failureCode: 'frame_interpolation_component_invalid', failureDetail: 'pending runtime file is damaged' });
assert.match(label.textContent, /pending runtime file is damaged/);
assert.equal(cancelInstall.disabled, false);
assert.equal(install.disabled, true);
assert.equal(restart.disabled, true);
assert.equal(restartConfirmation.classList.contains('hidden'), true);
render({ installed: true, restartRequired: true, pendingOperation: 'uninstall' }, { failureCode: 'frame_interpolation_component_busy', failureDetail: 'other player running' });
assert.match(label.textContent, /未能执行组件变更/);
assert.match(label.textContent, /other player running/);
assert.equal(cancelInstall.disabled, true);
assert.equal(pendingInstall.classList.contains('hidden'), true);
assert.equal(restartConfirmation.classList.contains('hidden'), true, 'uninstall does not use the install restart flow');
render({ installed: false, error: { code: 'frame_interpolation_component_not_published', message: '组件包尚未发布' } });
assert.equal(label.dataset.state, 'error');
assert.equal(install.disabled, false);
assert.equal(timers.size, 0);
render({ installed: true }, { failureCode: 'gpu_unsupported', failureDetail: 'GPU 不支持' });
assert.equal(install.disabled, true);
assert.equal(uninstall.disabled, false);
assert.equal(label.dataset.state, 'error');
render({ installed: true }, { componentStatus: 'ready', gpuName: 'RTX', runtimeVersion: '1.4.0.76', models: [{}, {}, {}], engineCount: 0 });
assert.equal(label.dataset.state, 'ready');
assert.match(label.textContent, /3 个模型/);
async function fireTimer() {
    assert.equal(timers.size, 1, 'exactly one pending status query');
    const [id, timer] = timers.entries().next().value;
    timers.delete(id);
    await timer.fn();
}
(async () => {
    response = { component: { installed: false, busy: true, progress: { phase: 'checking' } } };
    await context.changeFrameInterpolationComponent('frame_interpolation_component_install');
    assert.equal(requests[0][1], 'frame_interpolation_component_install');
    assert.deepEqual(Array.from(requests[0][2]), ['frame_interpolation_component_install']);
    response = { component: { installed: true, restartRequired: true, pendingOperation: 'uninstall' } };
    await context.changeFrameInterpolationComponent('frame_interpolation_component_uninstall');
    assert.equal(requests[1][1], 'frame_interpolation_component_uninstall');
    assert.match(label.textContent, /已排队卸载/);
    assert.equal(element('settings-confirm-uninstall-frame-interpolation').disabled, true);
    response = { component: { installed: false, restartRequired: false } };
    await context.changeFrameInterpolationComponent('frame_interpolation_component_cancel_install');
    assert.equal(requests[2][1], 'frame_interpolation_component_cancel_install');
    assert.deepEqual(Array.from(requests[2][2]), ['frame_interpolation_component_cancel_install']);
    assert.equal(install.disabled, false);
    assert.equal(restartConfirmation.classList.contains('hidden'), true);
    assert.equal(cancelInstall.disabled, true);
    assert.equal(pendingInstall.classList.contains('hidden'), true);

    render({ installed: false, busy: true, progress: { phase: 'downloading' } });
    requestFailure = new Error('transient IPC timeout');
    await fireTimer();
    assert.equal(timers.size, 1);
    assert.equal(timers.values().next().value.delay, 3000);
    assert.match(label.textContent, /transient IPC timeout/);
    assert.match(label.textContent, /正在重新查询/);
    assert.equal(install.disabled, true);
    await fireTimer();
    assert.equal(timers.size, 1, 'repeated failures do not lose or multiply the status timer');
    requestFailure = null;
    response = { component: { installed: false, busy: false, restartRequired: true, pendingOperation: 'install' } };
    await fireTimer();
    assert.equal(timers.size, 0);
    assert.match(label.textContent, /重新启动后生效/);
    assert.equal(cancelInstall.disabled, false);
    assert.equal(restartConfirmation.classList.contains('hidden'), false, 'completed download opens the restart confirmation');

    render({ installed: false, busy: true });
    response = { component: { installed: false, busy: false, error: { message: 'download failed' } } };
    await fireTimer();
    assert.equal(timers.size, 0);
    assert.equal(install.disabled, false);
    assert.equal(label.textContent, 'download failed');
    assert.equal(restartConfirmation.classList.contains('hidden'), true);

    requestFailure = new Error('operation acknowledgment timeout');
    await context.changeFrameInterpolationComponent('frame_interpolation_component_install');
    assert.equal(timers.size, 1, 'unknown operation result remains tracked until a status query succeeds');
    requestFailure = null;
    response = { component: { installed: false, busy: false } };
    await fireTimer();
    assert.equal(requests.at(-1)[1], 'frame_interpolation_status', 'retry queries status; never silently resubmits installation');
    assert.equal(timers.size, 0);
    assert.equal(install.disabled, false);
    const queuedInstall = { component: { installed: false, restartRequired: true, pendingOperation: 'install' } };
    response = queuedInstall;
    await context.refreshFrameInterpolationStatus();
    requestFailure = new Error('restart helper missing');
    const beforeFailure = requests.length;
    await context.restartForFrameInterpolationInstall();
    assert.equal(requests[beforeFailure][1], 'frame_interpolation_component_restart');
    assert.equal(requests.length, beforeFailure + 2, 'failure queries status once and never repeats restart');
    assert.equal(label.dataset.state, 'error');
    assert.match(label.textContent, /restart helper missing/);
    assert.equal(context.interpolationComponentRestarting, false);
    requestFailure = null;
    response = queuedInstall;
    await context.refreshFrameInterpolationStatus();
    await context.restartForFrameInterpolationInstall();
    assert.match(label.textContent, /重启助手未确认/);
    assert.equal(context.interpolationComponentRestarting, false);

    response = { restarting: true };
    const beforeRestart = requests.length;
    await context.restartForFrameInterpolationInstall();
    await context.restartForFrameInterpolationInstall();
    assert.equal(requests.length, beforeRestart + 1, 'double confirmation sends a single restart request');
    assert.deepEqual(Array.from(requests.at(-1)[2]), ['frame_interpolation_component_restart']);
    assert.equal(context.interpolationComponentRestarting, true);
    assert.equal(confirmRestart.disabled, true);
    assert.equal(cancelInstall.disabled, true);
    render(queuedInstall.component);
    assert.equal(confirmRestart.disabled, true, 'late status responses cannot re-enable restart');
    assert.match(label.textContent, /正在退出并重新启动/);
    console.log('RIFE_COMPONENT_UI_TEST_OK theme/install/uninstall/cancel-install/transient-status-retry/terminal-stop/restart-confirmation/later/restart-failure/single-restart');
})().catch((error) => { console.error(error); process.exitCode = 1; });
