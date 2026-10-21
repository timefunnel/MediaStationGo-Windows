// Exercise the embedded player's refresh scheduling and stale-response guards.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
function handler(name) {
    const start = source.search(new RegExp(`^    (?:async )?function ${name}\\(`, 'm'));
    assert.ok(start >= 0, `missing handler ${name}`);
    const next = source.slice(start + 1).search(/^    (?:async )?function /m);
    assert.ok(next >= 0);
    return source.slice(start, start + 1 + next);
}
function harness(respond) {
    const requests = [], timers = [], warnings = [], errors = [];
    const context = vm.createContext({
        player: {
            card: { id: 'media-a' }, started: true, exiting: false,
            loadInfo: { mediaMetadataPending: false }, metadataRefreshLoadInfo: null,
        },
        window: { setTimeout(resolve, delay) { timers.push({ resolve, delay }); } },
        console: { warn: (message) => warnings.push(message), error: (message) => errors.push(message) },
        refreshPlayerTools() {}, renderPlayerPanel() {}, friendlyError: (error) => error.message,
        nativeRequest: async (channel, operation, args) => {
            requests.push({ channel, operation, args });
            return respond(requests.length);
        },
    });
    for (const name of ['applyPlaybackMetadataRefresh', 'schedulePlaybackMetadataRefresh', 'refreshPlaybackMetadata']) {
        vm.runInContext(handler(name), context);
    }
    return { context, requests, timers, warnings, errors };
}
const ready = { mediaMetadataPending: false, subtitleEnabled: true, subtitleTrackKey: 'stream:2', subtitleRestorePending: false };
const flush = async () => { for (let index = 0; index < 8; index++) await Promise.resolve(); };
async function advance(h) {
    assert.ok(h.timers.length > 0, 'expected a bounded retry');
    h.timers.shift().resolve();
    await flush();
}
async function main() {
    {
        const h = harness(() => ready);
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        assert.equal(h.requests.length, 1, 'Started and load acknowledgement share one refresh');
        h.context.player.loadInfo = { mediaMetadataPending: false };
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        assert.equal(h.requests.length, 2, 'same-media reload must restore tracks again');
        assert.equal(h.context.player.loadInfo.subtitleTrackKey, 'stream:2');
        assert.equal(h.timers.length, 0);
    }
    {
        let release;
        const h = harness(() => new Promise((resolve) => { release = resolve; }));
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        const replacement = { subtitleEnabled: false };
        h.context.player.loadInfo = replacement;
        release(ready);
        await flush();
        assert.equal(replacement.subtitleEnabled, false, 'old reply must not mutate reloaded playback');
        assert.equal(replacement.subtitleTrackKey, undefined);
    }
    {
        const h = harness((attempt) => attempt === 1 ? { ...ready, subtitleEnabled: false, subtitleRestorePending: true } : ready);
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        await advance(h);
        assert.equal(h.requests.length, 2, 'subtitle pending must retry even when video/audio metadata is ready');
        assert.equal(h.context.player.loadInfo.subtitleEnabled, true);
    }
    {
        const h = harness((attempt) => { if (attempt === 1) throw new Error('track query failed'); return ready; });
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        await advance(h);
        assert.equal(h.requests.length, 2, 'transient track failure must not mark subtitle recovery complete');
        assert.equal(h.errors.length, 1);
        assert.equal(h.warnings.length, 0);
    }
    {
        const h = harness(() => ({ ...ready, subtitleRestorePending: true }));
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        for (let index = 0; index < 3; index++) await advance(h);
        assert.equal(h.requests.length, 4, 'unavailable subtitle recovery stays bounded');
        assert.equal(h.timers.length, 0);
        assert.match(h.warnings[0], /subtitle_pending=true/);
    }
    {
        const h = harness(() => ({ ...ready, subtitleEnabled: false, subtitleTrackKey: null }));
        h.context.player.interpolationChanging = true;
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        assert.equal(h.requests.length, 0, 'reload in progress must not refresh the previous load');
        h.context.player.interpolationChanging = false;
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        assert.equal(h.context.player.loadInfo.subtitleEnabled, false, 'disabled subtitles are a completed state');
        assert.equal(h.requests.length, 1);
        assert.equal(h.timers.length, 0);
    }
    {
        const h = harness(() => ({ ...ready, subtitleRestorePending: true }));
        h.context.schedulePlaybackMetadataRefresh(h.context.player);
        await flush();
        h.context.player.loadInfo = {};
        await advance(h);
        assert.equal(h.requests.length, 1, 'old retry must stop after same-media reload');
        assert.equal(h.timers.length, 0);
    }
    console.log('PLAYBACK_TRACK_REFRESH_OK per-load/late-response/subtitle-pending/transient-error/bounded-retries/disabled/reload-cancellation');
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
