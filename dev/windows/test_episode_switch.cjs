// Replay native events during the real episode-switch handlers; no server or media I/O.
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { handler } = require('./video_components_test_dom.cjs');

function harness({ resume = 450000, intro = 90, outro = 90, automatic = false } = {}) {
    const loads = [], seeks = [], failures = [], advances = [], feedback = [];
    let paint, resolveLoad, rejectLoad, finished = 0;
    const previous = { id: 'episode-old', type: 'Episode', playable: true };
    const episode = { id: 'episode-next', type: 'Episode', playable: true,
        resumePositionMs: resume, durationMs: 2400000 };
    const context = vm.createContext({
        player: { card: previous, loadInfo: {}, started: true, playing: true,
            positionMs: 1200000, durationMs: 2500000,
            seriesPlaybackSetting: { introSkipSeconds: intro, outroSkipSeconds: outro, autoNext: true } },
        playerPlaybackRate: 1, preferredInterpolationModel: 'rife-v4.26', playerSubtitleStyle: { fontSize: 40 },
        window: { jmpNative: { playerPause() {}, playerPlay() {}, playerSeek(ms) { seeks.push(ms); } } },
        byId: () => ({}), episodeSeasonNumber: () => 1, playbackPreferenceScope: () => 'series',
        playerSubtitlePosition: () => 100, cardTitle: () => '', episodePosition: () => '', formatTime: String,
        applyPlayerPlaybackRate() {}, setPlayerPoster() {}, updatePlayerProgress() {}, closePlayerPanel() {},
        setPlayerLoading() {}, playerTransitionLoadingLabel: () => '', refreshPlayerTools() {},
        waitForPlayerPaint: () => new Promise(resolve => { paint = resolve; }),
        nativeRequest(channel, operation, args) {
            assert.equal(operation, 'load');
            loads.push(args.slice(0, 2));
            return new Promise((resolve, reject) => { resolveLoad = resolve; rejectLoad = reject; });
        },
        schedulePlaybackMetadataRefresh() {}, hidePlayerFeedback() {}, hidePlayerControls() {}, showPlayerControls() {},
        showPlayerFeedback: text => feedback.push(text), showToast: text => feedback.push(text),
        showPlaybackFailure: text => failures.push(text), friendlyError: error => error.message,
        finishPlayer() { finished++; },
        danmakuRenderer: { updatePlayback() {} }, console,
        advanceToNextEpisode: (...args) => advances.push(args),
    });
    for (const name of ['switchPlayerEpisode', 'handlePlaybackEvent', 'maybeHandleEpisodeAutomation', 'validateEpisodeAutomationBounds']) {
        vm.runInContext(handler(name), context);
    }
    const transition = context.switchPlayerEpisode(episode, { automatic });
    const emit = (kind, fields = {}, mediaId = episode.id) => context.handlePlaybackEvent({ kind, mediaId, ...fields });
    return { context, previous, episode, loads, seeks, failures, advances, feedback, transition, emit,
        get finished() { return finished; },
        async painted() { paint(); await new Promise(setImmediate); },
        resolve() { resolveLoad({}); return transition; },
        reject() { rejectLoad(new Error('test load rejected')); return transition; },
    };
}

function staleEvents(h) {
    const before = JSON.stringify(h.context.player);
    for (const kind of ['position', 'seeked', 'duration', 'started', 'paused', 'buffering', 'finished', 'error', 'canceled']) {
        h.emit(kind, { positionMs: 2450000, durationMs: 2500000, buffering: true, seeking: false }, h.previous.id);
        assert.equal(JSON.stringify(h.context.player), before, `old ${kind} must not change the target episode`);
    }
    assert.equal(h.finished, 0);
    assert.equal(h.failures.length, 0);
    assert.equal(h.advances.length, 0);
    assert.equal(h.seeks.length, 0);
}

async function main() {
    // Old events arrive both before the load IPC and while the native load is pending.
    for (const resume of [0, 450000]) {
        const h = harness({ resume });
        staleEvents(h);
        await h.painted();
        assert.equal(h.loads[0][0], h.episode.id);
        assert.equal(h.loads[0][1], resume, 'manual next uses its own history, including zero');
        staleEvents(h);
        h.emit('started');
        assert.equal(h.context.player.episodeChanging, false);
        assert.equal(await h.resolve(), true);
        staleEvents(h);
        h.emit('position', { positionMs: Math.max(resume, 90000), durationMs: 2400000, seeking: false });
        assert.equal(h.context.player.positionMs, Math.max(resume, 90000));
        assert.equal(h.context.player.durationMs, 2400000);
    }
    // A load acknowledgement may precede Started; neither order drops current events.
    {
        const h = harness();
        await h.painted();
        assert.equal(await h.resolve(), true);
        staleEvents(h);
        h.emit('started');
        h.emit('seeked', { positionMs: 460000 });
        assert.equal(h.context.player.positionMs, 460000);
        assert.equal(h.context.player.started, true);
    }
    // Intro skipping still applies to the new episode's progress, not the previous episode's tail.
    for (const [resume, expectedSeeks] of [[0, [90000]], [30000, [90000]], [450000, []]]) {
        const h = harness({ resume });
        await h.painted();
        await h.resolve();
        h.emit('started');
        h.emit('position', { positionMs: resume, durationMs: 2400000, seeking: false });
        assert.deepEqual(h.seeks, expectedSeeks);
        assert.equal(h.context.player.introSkipHandled, true);
        h.emit('position', { positionMs: Math.max(resume, 90000), seeking: false });
        assert.deepEqual(h.seeks, expectedSeeks, 'intro is skipped once per episode');
    }
    {
        const h = harness({ resume: 30000, intro: 0, outro: 0 });
        await h.painted(); await h.resolve(); h.emit('started');
        h.emit('position', { positionMs: 30000, seeking: false });
        assert.equal(h.seeks.length, 0, 'disabled intro/outro must preserve the target history');
    }
    // Automatic continuation keeps its existing start policy and shares old-event isolation.
    {
        const h = harness({ automatic: true });
        staleEvents(h);
        await h.painted();
        assert.equal(h.loads[0][1], 0);
        await h.resolve(); h.emit('started');
        staleEvents(h);
        h.emit('position', { positionMs: 2310000, durationMs: 2400000, seeking: false });
        assert.equal(h.advances.length, 1, 'only the current episode can trigger outro advancement');
    }
    // A failed switch restores the previous episode and accepts its subsequent native events.
    {
        const h = harness();
        await h.painted();
        assert.equal(await h.reject(), false);
        assert.equal(h.context.player.card, h.previous);
        assert.equal(h.context.player.positionMs, 1200000);
        h.emit('position', { positionMs: 1205000 }, h.previous.id);
        assert.equal(h.context.player.positionMs, 1205000);
        const before = JSON.stringify(h.context.player);
        h.emit('position', { positionMs: 450000 });
        assert.equal(JSON.stringify(h.context.player), before);
        assert.equal(h.feedback.length, 1, 'load failure remains visible');
    }
    console.log('EPISODE_SWITCH_OK history/zero/stale-events/load-order/intro/outro/automatic/rollback');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
