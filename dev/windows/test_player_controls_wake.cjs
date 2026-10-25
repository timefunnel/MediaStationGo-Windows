// Exercise the real controls handlers with pointer jitter and native state notifications.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { handler, node } = require('./video_components_test_dom.cjs');
const source = fs.readFileSync(require('node:path').join(__dirname, '../../src/web/mediastation.js'), 'utf8');

function harness() {
    const controls = node(), view = node(), elements = new Map(), timers = new Map();
    controls.contains = () => false;
    let nextTimer = 0;
    const context = vm.createContext({
        player: { card: { id: 'movie', type: 'Movie' }, started: true, playing: true,
            positionMs: 450000, durationMs: 2400000, buffering: false },
        playerControls: controls, playerView: view, controlsTimer: 0,
        document: { activeElement: null },
        byId(id) { if (!elements.has(id)) elements.set(id, node()); return elements.get(id); },
        window: { _isFullscreen: true,
            setTimeout(fn, delay) { const id = ++nextTimer; timers.set(id, { fn, delay }); return id; },
            clearTimeout(id) { timers.delete(id); } },
        refreshPlayerTools() {}, updatePlayerProgress() {}, setPlayerLoading() {},
        schedulePlaybackMetadataRefresh() {}, hidePlayerFeedback() {},
        danmakuRenderer: { updatePlayback() {} },
        focusElement(target) { context.document.activeElement = target; },
    });
    for (const name of ['showPlayerControls', 'hidePlayerControls', 'refreshPlayerCursor',
        'scheduleControlsHide', 'isDanmakuConfigurationActive', 'handlePlaybackEvent', 'handlePlayerMouseMove']) {
        vm.runInContext(handler(name), context);
    }
    const registration = source.match(/^    playerView\.addEventListener\('mousemove'.+$/m);
    assert.ok(registration);
    vm.runInContext(registration[0], context);
    const leaveRegistration = source.match(/^    playerView\.addEventListener\('mouseleave'.+$/m);
    assert.ok(leaveRegistration);
    vm.runInContext(leaveRegistration[0], context);
    return { context, controls, view, timers,
        visible: () => controls.classList.contains('visible'),
        move: (x, y) => view.events.mousemove({ clientX: x, clientY: y }),
        emit: (kind, fields = {}) => context.handlePlaybackEvent({ kind, mediaId: 'movie', ...fields }),
        expire() {
            const entry = [...timers.entries()].find(([, timer]) => timer.delay === 3000);
            assert.ok(entry, 'controls hide timer must be armed');
            timers.delete(entry[0]); entry[1].fn();
        },
    };
}

{
    const h = harness();
    h.move(100, 100); h.context.hidePlayerControls();
    for (const x of [100, 101, 100, 99, 100, 102, 100]) {
        h.move(x, 100);
        assert.equal(h.visible(), false, 'stationary events and pointer jitter must not wake controls');
    }
    h.move(106, 100);
    assert.equal(h.visible(), true, 'deliberate movement wakes controls');
    const timer = h.context.controlsTimer;
    h.move(106, 100); h.move(107, 100); h.move(106, 100);
    assert.equal(h.context.controlsTimer, timer, 'jitter must not extend the visible timeout');
    h.move(112, 100);
    assert.notEqual(h.context.controlsTimer, timer, 'deliberate movement refreshes the timeout');
    h.expire(); assert.equal(h.visible(), false);
    assert.equal(h.controls.inert, true);
}
{
    const h = harness(); h.move(100, 100);
    for (let x = 101; x < 106; x++) { h.move(x, 100); assert.equal(h.visible(), false); }
    h.move(106, 100);
    assert.equal(h.visible(), true, 'slow intentional movement accumulates displacement');
    h.move(111, 100); h.context.hidePlayerControls();
    h.move(112, 100);
    assert.equal(h.visible(), false, 'hiding anchors the threshold at the latest pointer position');
    h.move(117, 100); assert.equal(h.visible(), true);
}
{
    const h = harness(); h.move(100, 100);
    for (let index = 0; index < 50; index++) { h.move(101, 101); h.move(99, 99); }
    assert.equal(h.visible(), false, 'back-and-forth jitter does not accumulate distance');
    h.view.events.mouseleave();
    h.move(800, 500); assert.equal(h.visible(), false, 're-entry establishes a fresh pointer baseline');
    h.move(806, 500); assert.equal(h.visible(), true);
}
{
    const h = harness();
    h.emit('started'); assert.equal(h.visible(), false, 'ordinary Started must preserve hidden controls');
    h.context.showPlayerControls();
    h.emit('started'); assert.equal(h.visible(), true);
    h.expire(); assert.equal(h.visible(), false, 'visible controls still hide after resume');
    h.emit('buffering', { buffering: true }); h.emit('buffering', { buffering: false });
    assert.equal(h.visible(), false, 'buffering notifications do not wake controls');
    h.context.showPlayerControls(); h.emit('buffering', { buffering: true });
    h.expire(); assert.equal(h.visible(), true);
    h.emit('buffering', { buffering: false }); h.expire();
    assert.equal(h.visible(), false, 'buffering completion restores the existing hide timer');
}
{
    const h = harness();
    h.emit('paused'); assert.equal(h.visible(), true, 'pause remains an explicit controls trigger');
    assert.equal(h.timers.size, 0, 'paused controls remain visible');
    h.context.player.playing = true; h.context.player.started = false;
    h.emit('started'); assert.equal(h.visible(), false, 'the first frame keeps its existing hide behavior');
    h.context.showPlayerControls('exit');
    assert.equal(h.visible(), true); assert.equal(h.context.player.exitArmed, true);
    assert.equal(h.context.document.activeElement, h.context.byId('player-exit'));
    h.context.player.exiting = true;
    h.move(100, 100); h.context.hidePlayerControls(); h.move(1000, 1000);
    assert.equal(h.visible(), false, 'exit ignores pointer events');
    h.context.player = null; h.move(0, 0);
}
console.log('PLAYER_CONTROLS_WAKE_OK stationary/jitter/threshold/slow-movement/timeout/started/buffering/pause/keyboard/exit');
