// Exercise the embedded player's real event handlers with native callbacks and a clock.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const elements = new Map();
function element(id) {
    if (!elements.has(id)) {
        const classes = new Set();
        const feedbackText = { textContent: '' };
        const feedbackBody = {
            dataset: {},
            querySelector(selector) { return selector === '.player-feedback-text' ? feedbackText : null; },
        };
        elements.set(id, {
            inert: false, keyboardFocus: false,
            classList: {
                add(name) { classes.add(name); },
                remove(name) { classes.delete(name); },
                contains(name) { return classes.has(name); },
                toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); },
            },
            querySelector(selector) { return selector === '.player-feedback-body' ? feedbackBody : (this.keyboardFocus ? {} : null); },
            contains() { return false; },
            matches() { return false; },
        });
    }
    return elements.get(id);
}
const timers = new Map();
let nextTimer = 0;
const nativeCalls = [];
let finishedPlayers = 0;
let episodeAdvances = 0;
const context = vm.createContext({
    byId: element, playerControls: element('player-controls'), playerView: element('player-view'),
    player: { started: true, playing: true, positionMs: 15000, durationMs: 60000 },
    controlsTimer: 0, feedbackTimer: 0,
    document: { activeElement: null },
    window: {
        clearTimeout(id) { timers.delete(id); },
        setTimeout(fn, delay) { const id = ++nextTimer; timers.set(id, { fn, delay }); return id; },
        jmpNative: {
            playerPause() { nativeCalls.push('pause'); },
            playerPlay() { nativeCalls.push('play'); },
        },
    },
    refreshPlayerTools() {}, updatePlayerProgress() {}, setPlayerLoading() {},
    schedulePlaybackMetadataRefresh() {},
    finishPlayer() { finishedPlayers += 1; },
    advanceToNextEpisode() { episodeAdvances += 1; },
    danmakuRenderer: { updatePlayback() {} },
});
for (const name of ['togglePlayback', 'handlePlaybackEvent', 'showPlayerControls',
    'hidePlayerControls', 'refreshPlayerCursor', 'showPlayerFeedback', 'hidePlayerFeedback',
    'scheduleControlsHide', 'isDanmakuConfigurationActive']) {
    const start = source.indexOf(`    function ${name}(`);
    assert.ok(start >= 0, `missing player handler ${name}`);
    const next = source.slice(start + 1).search(/^    (?:async )?function /m);
    assert.ok(next >= 0, `missing function boundary after ${name}`);
    vm.runInContext(source.slice(start, start + 1 + next), context);
}
element('player-more-menu').classList.add('hidden');
element('player-feedback').classList.add('hidden');
function expire(delay) {
    const entry = [...timers.entries()].find(([, timer]) => timer.delay === delay);
    assert.ok(entry, `missing ${delay} ms timer`);
    timers.delete(entry[0]); entry[1].fn();
}
function checkFeedback(icon) {
    assert.equal(element('player-feedback').querySelector('.player-feedback-body').dataset.icon, icon);
    assert.equal(element('player-feedback').classList.contains('hidden'), false);
}
context.togglePlayback();
assert.equal(nativeCalls.at(-1), 'pause');
checkFeedback('pause');
context.handlePlaybackEvent({ kind: 'paused' });
checkFeedback('pause');
expire(750);
assert.equal(element('player-feedback').classList.contains('hidden'), true);
assert.equal(context.playerControls.classList.contains('visible'), true);
assert.equal(context.playerControls.inert, false);
assert.equal([...timers.values()].some((timer) => timer.delay === 3000), false);

context.togglePlayback();
assert.equal(nativeCalls.at(-1), 'play');
context.handlePlaybackEvent({ kind: 'started' });
checkFeedback('play');
expire(750);
assert.equal(element('player-feedback').classList.contains('hidden'), true);
expire(3000);
assert.equal(context.playerControls.classList.contains('visible'), false);
assert.equal(context.playerControls.inert, true);

const resumeCalls = [];
context.startPlayback = (...args) => resumeCalls.push(args);
{
    const start = source.indexOf('    function resumePlayback(');
    const next = source.slice(start + 1).search(/^    (?:async )?function /m);
    vm.runInContext(source.slice(start, start + 1 + next), context);
}
const resumeCard = { id: 'resume-item', resumePositionMs: 42000 };
context.resumePlayback(resumeCard);
assert.deepEqual(resumeCalls[0], [resumeCard, 42000]);
assert.equal((source.match(/onClick: resumePlayback/g) || []).length, 2, 'home and full resume views must both start playback directly');

context.player.buffering = true;
context.showPlayerControls();
context.hidePlayerControls();
assert.equal(context.playerControls.classList.contains('visible'), true);
assert.equal(context.playerControls.inert, false);
context.player.buffering = false;
context.playerControls.keyboardFocus = true;
context.scheduleControlsHide();
expire(3000);
assert.equal(context.playerControls.classList.contains('visible'), true);
context.playerControls.keyboardFocus = false;
context.scheduleControlsHide();
expire(3000);
assert.equal(context.playerControls.inert, true);

context.showPlayerControls();
element('player-more-menu').classList.remove('hidden');
context.scheduleControlsHide();
context.hidePlayerControls();
assert.equal(context.playerControls.classList.contains('visible'), true);
assert.equal([...timers.values()].some((timer) => timer.delay === 3000), false);
context.player.exiting = true;
context.player.card = { type: 'Episode' };
context.player.seriesPlaybackSetting = { autoNext: true };
context.handlePlaybackEvent({ kind: 'finished' });
assert.equal(finishedPlayers, 1);
assert.equal(episodeAdvances, 0);
console.log('MODERN_CINEMA_PLAYER_UI_TEST_OK: native pause/play feedback expires; idle controls become inert; buffering, keyboard focus and menus retain controls; confirmed exit does not auto-advance');

// Settings must survive direct loading renders as well as normal navigation.
function handlerSource(name) {
    const start = Math.max(source.indexOf(`    function ${name}(`), source.indexOf(`    async function ${name}(`));
    assert.ok(start >= 0, `missing handler ${name}`);
    const next = source.slice(start + 1).search(/^    (?:async )?function /m);
    return source.slice(start, start + 1 + next);
}
function node() {
    return { children: [], classList: { remove() {} }, attrs: {},
        append(...children) { this.children.push(...children); },
        setAttribute(key, value) { this.attrs[key] = value; } };
}
const settingsNode = node(); let settingsDestroyed = false;
const contentNode = node(); contentNode.children.push(settingsNode);
contentNode.contains = (item) => contentNode.children.includes(item);
contentNode.replaceChildren = (...children) => {
    if (contentNode.contains(settingsNode)) settingsDestroyed = true;
    contentNode.children = children;
};
const bodyNode = node(); bodyNode.append = (item) => {
    contentNode.children = contentNode.children.filter((child) => child !== item);
    bodyNode.children.push(item);
};
const lifecycle = vm.createContext({
    content: contentNode, document: { body: bodyNode },
    byId: () => settingsDestroyed ? null : settingsNode, element: node,
});
for (const name of ['parkSettings', 'replaceContent', 'renderLoading']) vm.runInContext(handlerSource(name), lifecycle);
lifecycle.renderLoading();
assert.equal(settingsDestroyed, false);
assert.equal(bodyNode.children[0], settingsNode);
assert.equal(settingsNode.attrs.role, 'dialog');
assert.equal(contentNode.children[0].children.length, 12);
lifecycle.renderLoading();
assert.equal(bodyNode.children.length, 1, 'settings controls must not be cloned');

// The actual rendered image footprint determines whether artwork is sharp enough.
const heroContext = vm.createContext({ content: { clientWidth: 1064 }, window: { devicePixelRatio: 1.25 }, Math });
vm.runInContext(handlerSource('heroArtworkFits'), heroContext);
const heroFrame = { clientWidth: 1000, clientHeight: 300 };
assert.equal(heroContext.heroArtworkFits(heroFrame, { width: 1920, height: 1080 }), true);
assert.equal(heroContext.heroArtworkFits(heroFrame, { width: 640, height: 360 }), false);
assert.equal(heroContext.heroArtworkFits(heroFrame, { width: 1200, height: 1800 }), false);
heroContext.window.devicePixelRatio = 3;
assert.equal(heroContext.heroArtworkFits(heroFrame, { width: 1280, height: 720 }), false);
heroContext.content.clientWidth = 375; heroContext.window.devicePixelRatio = 2;
assert.equal(heroContext.heroArtworkFits({ clientWidth: 343 }, { width: 1280, height: 720 }), true);
console.log('MODERN_CINEMA_LAYOUT_TEST_OK: settings survive direct loading navigation without duplicate controls; artwork checks actual footprint, density and aspect ratio');

// A failure uses the existing cleanup path and keeps late callbacks from reviving playback.
const failedCard = { id: 'failed-movie', title: '测试影片', type: 'Movie' };
const modes = [];
context.player = { card: failedCard, positionMs: 23000, mediaSourceId: 'source-1', exiting: false };
context.playbackFailure = null;
context.cardTitle = (card) => card.title;
context.setPlayerMode = (enabled) => modes.push(enabled);
context.finishPlayer = () => { finishedPlayers += 1; context.player = null; };
element('player-view').classList.remove('hidden');
element('player-failure').classList.add('hidden');
element('player-retry').focus = () => { context.document.activeElement = element('player-retry'); };
for (const name of ['showPlaybackFailure', 'dismissPlaybackFailure']) vm.runInContext(handlerSource(name), context);
context.handlePlaybackEvent({ kind: 'error', message: '播放地址不可用' });
assert.equal(context.player, null);
assert.equal(context.playbackFailure.card.id, failedCard.id);
assert.equal(context.playbackFailure.positionMs, 23000);
assert.equal(context.playbackFailure.mediaSourceId, 'source-1');
assert.equal(context.playerControls.inert, true);
assert.equal(element('player-failure').classList.contains('hidden'), false);
assert.equal(context.document.activeElement, element('player-retry'));
context.handlePlaybackEvent({ kind: 'started' });
assert.equal(context.player, null, 'late start callback cannot revive a failed player');
context.dismissPlaybackFailure();
assert.equal(context.playbackFailure, null);
assert.equal(element('player-failure').classList.contains('hidden'), true);
assert.equal(modes.at(-1), false);
console.log('MODERN_CINEMA_FAILURE_TEST_OK: cleanup precedes failure UI; retry retains position and source; controls stay inert; late callbacks cannot revive playback');

function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
const settle = () => new Promise((resolve) => setImmediate(resolve));

async function checkImageScheduling() {
    const requests = [];
    const images = vm.createContext({
        imageCache: new Map(), imagePending: new Map(), imageQueue: [],
        imageActive: 0, imageBackgroundActive: 0,
        nativeRequest(_channel, _action, args) {
            const request = { key: JSON.parse(args[0]).key, ...deferred() };
            requests.push(request); return request.promise;
        },
        updateImageCacheStatus() {}, scheduleImageCacheStatsRefresh() {}, console,
    });
    for (const name of ['requestImage', 'imageMemoryKey', 'pumpImages']) vm.runInContext(handlerSource(name), images);
    const request = (key, background = false) => images.requestImage({ key }, 1280, { background });
    const complete = (key) => requests.find((entry) => entry.key === key).resolve({ key, dataUrl: 'data:image/png;base64,AA==' });
    const hero = request('slow-hero', true);
    const nextHero = request('next-hero', true);
    const cover = request('visible-cover');
    const nextCover = request('next-visible-cover');
    assert.deepEqual(requests.map((entry) => entry.key), ['slow-hero', 'visible-cover']);
    assert.equal(images.imageActive, 2);
    assert.equal(images.imageBackgroundActive, 1);
    complete('visible-cover'); await cover; await settle();
    assert.equal(requests.at(-1).key, 'next-visible-cover', 'visible covers must run ahead of queued hero artwork');
    complete('next-visible-cover'); await nextCover; await settle();
    assert.equal(images.imageActive, 1, 'a slow hero must leave one slot available for covers');
    assert.equal(requests.length, 3, 'only one background image may be active');
    const promoted = request('next-hero');
    assert.equal(promoted, nextHero, 'foreground promotion must reuse the existing request');
    assert.equal(requests.at(-1).key, 'next-hero');
    complete('next-hero'); await promoted; await settle();
    complete('slow-hero'); await hero; await settle();
    assert.equal(images.imageActive, 0); assert.equal(images.imageBackgroundActive, 0);
    const failure = request('failed-image');
    const rejected = assert.rejects(failure, /actual image failure/);
    requests.at(-1).reject(new Error('actual image failure')); await rejected; await settle();
    assert.equal(images.imagePending.size, 0);
    assert.equal(images.imageCache.has('failed-image@1280'), false);

    const candidates = [1, 2, 3, 4, 5].map((id) => ({ id: String(id) }));
    const artRequests = []; let reveals = 0;
    const heroNode = { isConnected: true, dataset: {} };
    const artwork = vm.createContext({
        heroRevision: 7, heroCarouselMaxCards: 4,
        loadHeroArtwork(_hero, card) { const task = { card, ...deferred() }; artRequests.push(task); return task.promise; },
        heroArtworkFits() { return true; },
        refreshHeroArtworkQualification() { reveals += 1; heroNode.dataset.qualifiedIds = heroNode._artworkPool.map((entry) => entry.card.id).join(','); },
        console, friendlyError: String,
    });
    vm.runInContext(handlerSource('loadHeroCandidates'), artwork);
    const loading = artwork.loadHeroCandidates(heroNode, candidates, 7);
    assert.equal(artRequests.length, 1);
    artRequests[0].resolve({ width: 1920, height: 1080 }); await settle();
    assert.equal(reveals, 1, 'the first sharp hero should display before other hero images finish');
    for (let index = 1; index < 4; index += 1) { artRequests[index].resolve({ width: 1920, height: 1080 }); await settle(); }
    await loading;
    assert.equal(artRequests.length, 4, 'stop downloading candidates once the carousel is complete');
    assert.equal(reveals, 2);
    console.log('MODERN_CINEMA_IMAGES_TEST_OK: stalled hero does not block covers; foreground priority and deduplication preserve the two-request bound; first sharp artwork appears immediately');
}

async function checkFavoritesRefresh() {
    const requests = []; const renders = [];
    const favorites = vm.createContext({
        currentView: null, libraryPageSize: 50, homeData: null, libraryCache: new Map(), seriesDetailCache: new Map(),
        nativeRequest(_channel, action, args) { const task = { action, params: JSON.parse(args[1]), ...deferred() }; requests.push(task); return task.promise; },
        validateLibraryPage: (page) => page, friendlyError: String,
        renderFavorites(data) { renders.push(data); },
        seriesDetailCacheKey: (id) => id, showToast(message) { throw new Error(message); },
        document: { querySelectorAll: () => [] },
        element() { return { dataset: {}, attrs: {}, setAttribute(key, value) { this.attrs[key] = value; },
            removeAttribute(key) { delete this.attrs[key]; }, addEventListener(_event, handler) { this.click = handler; } }; },
    });
    for (const name of ['loadFavorites', 'refreshFavoritesAfterEntry', 'createFavoriteButton']) vm.runInContext(handlerSource(name), favorites);
    const data = { items: [{ id: 'old-cache' }], totalRecordCount: 1, loading: false, error: '' };
    favorites.currentView = { kind: 'favorites', data };
    favorites.refreshFavoritesAfterEntry({ kind: 'detail', data: {} });
    assert.equal(requests.at(-1).params.startIndex, 0, 'returning from details must refresh the first page');
    assert.equal(data.items.length, 0, 'restored history must not retain an obsolete favorites list');
    const stale = requests.at(-1);
    const freshLoad = favorites.loadFavorites(data, { refresh: true });
    const fresh = requests.at(-1);
    stale.resolve({ items: [{ id: 'stale' }], totalRecordCount: 1 }); await settle();
    assert.equal(data.loading, true, 'an older request must not clear the fresh request loading state');
    assert.equal(data.items.length, 0);
    fresh.resolve({ items: [{ id: 'fresh' }], totalRecordCount: 1 }); await freshLoad;
    assert.equal(data.items[0].id, 'fresh'); assert.equal(data.loading, false);
    const older = favorites.loadFavorites(data, { refresh: true }); const olderTask = requests.at(-1);
    const newer = favorites.loadFavorites(data, { refresh: true }); const newerTask = requests.at(-1);
    newerTask.resolve({ items: [{ id: 'newest' }], totalRecordCount: 1 }); await newer;
    olderTask.reject(new Error('obsolete failure')); await older;
    assert.equal(data.items[0].id, 'newest'); assert.equal(data.error, '');

    // The user may navigate to Favorites before the successful favorite write returns.
    favorites.currentView = { kind: 'detail', data: {} };
    const card = { id: 'series', isFavorite: false };
    const button = favorites.createFavoriteButton(card);
    const mutation = button.click(); const write = requests.at(-1);
    favorites.currentView = { kind: 'favorites', data };
    favorites.refreshFavoritesAfterEntry({ kind: 'detail', data: {} }); const beforeWrite = requests.at(-1);
    write.resolve({ mediaId: 'series', isFavorite: true }); await mutation;
    assert.equal(card.isFavorite, true);
    assert.equal(requests.at(-1).action, 'favorites', 'a completed write must refresh an already open Favorites page');
    const afterWrite = requests.at(-1);
    assert.notEqual(beforeWrite, afterWrite);
    afterWrite.resolve({ items: [{ id: 'series' }], totalRecordCount: 1 }); await settle();
    beforeWrite.resolve({ items: [], totalRecordCount: 0 }); await settle();
    assert.equal(data.items[0].id, 'series', 'a pre-write response must not erase the refreshed list');
    const count = requests.length;
    favorites.refreshFavoritesAfterEntry({ kind: 'favorites', data });
    assert.equal(requests.length, count, 'rendering the same view must not trigger an extra fetch');
    favorites.currentView = { kind: 'home', data: {} };
    favorites.refreshFavoritesAfterEntry({ kind: 'favorites', data });
    assert.equal(requests.length, count);
    console.log('MODERN_CINEMA_FAVORITES_TEST_OK: history entry reloads; confirmed write refreshes an already open view; stale responses and failures cannot replace newer data');
}

(async () => { await checkImageScheduling(); await checkFavoritesRefresh(); })().catch((error) => {
    console.error(error); process.exitCode = 1;
});
