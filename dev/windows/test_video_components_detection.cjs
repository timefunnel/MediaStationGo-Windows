// Exercise first-entry component detection with the actual embedded UI handlers.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.html'), 'utf8');
function handler(name) {
    const match = new RegExp(`^    (?:async )?function ${name}\\(`, 'm').exec(source);
    assert.ok(match, name);
    const rest = source.slice(match.index + match[0].length);
    const next = /\n    (?:async )?function /.exec(rest);
    assert.ok(next, `next handler after ${name}`);
    return source.slice(match.index, match.index + match[0].length + next.index);
}
function node(tag = 'div') {
    const classes = new Set();
    return { tag, children: [], dataset: {}, disabled: false, textContent: '', events: {},
        append(...children) { this.children.push(...children); },
        replaceChildren(...children) { this.children = children; },
        setAttribute(name, value) { this[name] = value; },
        addEventListener(name, callback) { this.events[name] = callback; },
        querySelector() { return null; },
        classList: { add(name) { classes.add(name); }, remove(name) { classes.delete(name); },
            contains(name) { return classes.has(name); }, toggle(name, on) { on ? classes.add(name) : classes.delete(name); } },
    };
}
const elements = new Map();
function byId(id) {
    assert.ok(html.includes(`id="${id}"`), id);
    if (!elements.has(id)) elements.set(id, node());
    return elements.get(id);
}
const cards = new Map(['rtx-video', 'dlssnr', 'nvidia-vfg'].map((id) => [id, {
    querySelectorAll() { return []; }, querySelector() { return node(); },
}]));
const panel = node(), content = node(), toasts = [], requests = [];
let backend;
const context = vm.createContext({
    byId, document: { createElement: node, querySelector(selector) { return cards.get(selector.match(/"([^"]+)"/)[1]); } },
    window: { clearTimeout() {}, setTimeout() { return 1; } },
    nativeRequest: async (channel, operation, args) => {
        requests.push({ channel, operation, args });
        assert.equal(operation, 'video_components_status');
        return backend();
    },
    formatBytes: String, friendlyError: (error) => error.message,
    refreshPlayerTools() {}, refreshUpdateControls() {}, scheduleAutomaticUpdateCheck() {},
    refreshImageCacheStatus() {}, refreshFrameInterpolationStatus() {}, updateSessionUi() {},
    loadHome: async () => {}, splash: node(), loginView: node(), appShell: node(),
    closePlayerMore() {}, showToast: (message) => toasts.push(message),
    playerPanel: panel, playerPanelContent: content, playerControls: node(),
    controlsTimer: 0, positionPlayerPanel() {}, requestAnimationFrame(callback) { callback(); },
    interpolationModels: {},
});
const start = source.indexOf('    const videoComponentStatuses = new Map();');
vm.runInContext(source.slice(start, source.indexOf('    function openSettings(', start)), context);
const defaults = source.indexOf('    const videoEnhancementDefaults = ');
vm.runInContext(source.slice(defaults, source.indexOf('    async function setFrameInterpolation(', defaults)), context);
for (const name of ['element', 'createTrackOption', 'showApp', 'renderPlayerPanel', 'presentPlayerPanel', 'openPlayerPanel']) {
    vm.runInContext(handler(name), context);
}
const payload = { components: ['rtx-video', 'dlssnr', 'nvidia-vfg'].map((id) => ({
    id, supported: true, installed: id !== 'rtx-video',
    runtime: { playbackAvailable: id !== 'rtx-video', experimental: false },
})) };
function player() {
    context.player = { card: { id: 'local-test' }, loadInfo: {}, started: true, panelKind: '', interpolationEnabled: false, interpolationChanging: false };
    panel.classList.add('hidden'); content.replaceChildren();
}
function providerButtons() {
    return content.children.filter((child) => child.tag === 'button').slice(0, 3);
}
async function main() {
    backend = async () => payload;
    await context.showApp({});
    assert.equal(requests.length, 1, 'home entry must query native component status without opening component management');
    assert.equal(byId('player-interpolation').dataset.enhancementReady, 'true');
    assert.equal(byId('player-interpolation').classList.contains('hidden'), false);
    player(); context.player.panelKind = 'interpolation'; context.renderPlayerPanel();
    assert.deepEqual(providerButtons().map((button) => button.disabled), [true, false, false]);

    player(); vm.runInContext('videoComponentStatuses.clear()', context);
    let reply; backend = () => new Promise((resolve) => { reply = resolve; });
    const opening = context.openPlayerPanel('interpolation');
    assert.equal(content.children.length, 0, 'panel must wait for the current native status');
    reply(payload); await opening;
    assert.equal(panel.classList.contains('hidden'), false);
    assert.deepEqual(providerButtons().map((button) => button.disabled), [true, false, false]);
    const forms = content.children.filter((child) => child.tag === 'details');
    assert.deepEqual(forms.map((form) => form.children.at(-1).disabled), [true, false, false], 'installed provider apply buttons must be enabled');

    player(); backend = async () => { throw new Error('native IPC unavailable'); };
    await context.openPlayerPanel('interpolation');
    assert.equal(content.children.length, 0, 'query failure must not render providers as uninstalled');
    assert.match(toasts.at(-1), /组件状态查询失败/);
    assert.match(byId('component-dlssnr-status').textContent, /native IPC unavailable/);

    player(); backend = () => new Promise((resolve) => { reply = resolve; });
    const stale = context.openPlayerPanel('interpolation');
    const previous = context.player; context.player = null;
    reply(payload); await stale;
    assert.equal(content.children.length, 0, 'late status must not reopen an exited player');
    assert.equal(previous.panelKind, '');
    console.log('VIDEO_COMPONENT_DETECTION_OK home-first-entry/fresh-panel/enabled-apply/query-error/player-exit-race');
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
