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
    if (!elements.has(id)) elements.set(id, { textContent: '', disabled: false, dataset: {}, classList: { add() {} } });
    return elements.get(id);
};
let scheduled = 0;
let response;
let requests = [];
const context = vm.createContext({
    byId: element, interpolationModels: {}, interpolationComponentTimer: 0,
    window: { clearTimeout() { scheduled = 0; }, setTimeout() { scheduled += 1; return scheduled; } },
    formatBytes: (bytes) => `${bytes} B`, friendlyError: (error) => error.message,
    nativeRequest: async (...args) => { requests.push(args); return response; },
});
const start = source.indexOf('    function updateFrameInterpolationStatus(');
const end = source.indexOf('    function scheduleImageCacheStatsRefresh()', start);
assert.ok(start >= 0 && end > start);
vm.runInContext(source.substring(start, end), context);
const label = element('settings-frame-interpolation-status');
const install = element('settings-install-frame-interpolation');
const uninstall = element('settings-uninstall-frame-interpolation');
function render(component, fields = {}) {
    context.updateFrameInterpolationStatus({ componentStatus: 'unavailable', component, ...fields });
}
render({ installed: false });
assert.equal(install.disabled, false);
assert.equal(uninstall.disabled, true);
assert.match(label.textContent, /普通播放不受影响/);
render({ installed: false, busy: true, progress: { phase: 'downloading', downloadedBytes: 5, totalBytes: 9, source: 'GitHub 直连' } });
assert.equal(install.disabled, true);
assert.equal(uninstall.disabled, true);
assert.match(label.textContent, /5 B \/ 9 B · GitHub 直连/);
assert.equal(scheduled, 1);
render({ installed: false, restartRequired: true, pendingOperation: 'install' });
assert.equal(install.disabled, true);
assert.match(label.textContent, /重新启动后生效/);
assert.equal(scheduled, 0);
render({ installed: true, restartRequired: true, pendingOperation: 'uninstall' }, { failureCode: 'frame_interpolation_component_busy', failureDetail: 'other player running' });
assert.match(label.textContent, /未能执行组件变更/);
assert.match(label.textContent, /other player running/);
render({ installed: false, error: { code: 'frame_interpolation_component_not_published', message: '组件包尚未发布' } });
assert.equal(label.dataset.state, 'error');
assert.equal(install.disabled, false);
assert.equal(scheduled, 0);
render({ installed: true }, { failureCode: 'gpu_unsupported', failureDetail: 'GPU 不支持' });
assert.equal(install.disabled, true);
assert.equal(uninstall.disabled, false);
assert.equal(label.dataset.state, 'error');
render({ installed: true }, { componentStatus: 'ready', gpuName: 'RTX', runtimeVersion: '1.4.0.76', models: [{}, {}, {}], engineCount: 0 });
assert.equal(label.dataset.state, 'ready');
assert.match(label.textContent, /3 个模型/);
(async () => {
    response = { component: { installed: false, busy: true, progress: { phase: 'checking' } } };
    await context.changeFrameInterpolationComponent(true);
    assert.equal(requests[0][1], 'frame_interpolation_component_install');
    assert.deepEqual(Array.from(requests[0][2]), ['frame_interpolation_component_install']);
    response = { component: { installed: true, restartRequired: true, pendingOperation: 'uninstall' } };
    await context.changeFrameInterpolationComponent(false);
    assert.equal(requests[1][1], 'frame_interpolation_component_uninstall');
    assert.match(label.textContent, /已排队卸载/);
    assert.equal(element('settings-confirm-uninstall-frame-interpolation').disabled, true);
    console.log('RIFE_COMPONENT_UI_TEST_OK missing/busy/pending/failure/ready install/uninstall IPC states');
})().catch((error) => { console.error(error); process.exitCode = 1; });
