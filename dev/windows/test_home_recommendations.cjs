const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
function handler(name) {
    const match = new RegExp(`^    (?:async )?function ${name}\\(`, 'm').exec(source);
    assert.ok(match, name);
    const rest = source.slice(match.index + match[0].length);
    const next = /\n    (?:async )?function /.exec(rest);
    return source.slice(match.index, match.index + match[0].length + next.index);
}
function harness() {
    let now = 1000, calls = 0, loads = 0, timerId = 0;
    const timers = new Map(), listeners = new Map(), pending = [];
    const node = () => ({
        dataset: {}, isConnected: true, children: [], removed: false,
        append(...children) { this.children.push(...children); }, after() {},
        setAttribute() {}, addEventListener(event, callback) { this[event] = callback; },
        remove() { this.removed = true; },
    });
    const hero = node(); hero._placeholder = node();
    const context = vm.createContext({
        heroRevision: 1, heroRecommendationTimer: 0, heroRotationTimer: 0,
        heroRecommendationVisibility: () => {}, heroCarouselController: null,
        session: {}, homeRecommendationFailures: new WeakMap(), homeRecommendationRefreshes: new WeakMap(),
        imagePriorityHero: null, releaseHomeHeroImagePriority() {},
        Date: { now: () => now }, Number, Math, JSON, Array, Set,
        console: { error() {} }, friendlyError: error => error.message,
        document: { hidden: false, removeEventListener(event) { listeners.delete(event); } },
        window: {
            setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback() { timers.delete(id); callback(); }, delay }); return id; },
            clearTimeout(id) { timers.delete(id); },
        },
        nativeRequest(name, operation, args) {
            assert.equal(name, 'mediaStationCatalog'); assert.equal(operation, 'recommendations');
            calls++;
            return new Promise((resolve, reject) => pending.push({ resolve, reject, params: JSON.parse(args[1]) }));
        },
        element: node, showToast() {},
        async loadHeroCandidates(_hero, cards) { loads++; assert.deepEqual(Array.from(cards, card => card.id), ['recommended']); },
    });
    for (const name of ['heroCandidates', 'refreshHomeRecommendations', 'loadHomeRecommendations', 'stopHeroCarousel']) vm.runInContext(handler(name), context);
    return {
        context, hero, timers, pending,
        get calls() { return calls; }, get loads() { return loads; },
        setNow(value) { now = value; },
        resolve(expiresAtMs = now + 6 * 60 * 60 * 1000) {
            pending.shift().resolve({ items: [{ id: 'recommended', type: 'Movie', backdropImage: { itemId: 'recommended' } }], cache: { expiresAtMs } });
        },
        load: () => context.loadHomeRecommendations(hero, 1, 1440),
    };
}
const flush = () => new Promise(resolve => setImmediate(resolve));
async function main() {
    const active = harness();
    const first = active.load();
    await active.load(); assert.equal(active.calls, 1, 'overlapping loads must share the active request');
    active.resolve(); await first;
    assert.equal(active.loads, 1);
    const timer = [...active.timers.values()][0];
    assert.equal(timer.delay, 6 * 60 * 60 * 1000);
    active.setNow(1000 + timer.delay); timer.callback();
    assert.equal(active.calls, 2, 'an open home page must request a new group at expiration');
    active.resolve(); await flush();
    assert.equal(active.loads, 2);
    active.context.stopHeroCarousel();
    assert.equal(active.timers.size, 0, 'leaving home must cancel expiration timers');

    const away = harness();
    const obsolete = away.load(); away.context.stopHeroCarousel(); away.resolve(); await obsolete;
    assert.equal(away.loads, 0, 'old-account or detached home responses must not paint');
    assert.equal(away.timers.size, 0);

    const hidden = harness();
    const initial = hidden.load(); hidden.resolve(); await initial;
    hidden.context.document.hidden = true;
    [...hidden.timers.values()][0].callback();
    assert.equal(hidden.calls, 1, 'a minimized client must defer expired recommendations until visible');

    const failed = harness();
    const placeholder = failed.hero._placeholder;
    const request = failed.load(); failed.pending.shift().reject(new Error('connection failed')); await request;
    assert.ok(placeholder.removed, 'failure must not leave a permanent skeleton');
    assert.equal(failed.hero._recommendationStatus, undefined, 'failures must not add a banner prompt or retry button');
    await failed.load(); assert.equal(failed.calls, 1, 'failed recommendations must wait until the next launch');

    const expired = harness();
    const updating = expired.load(); expired.resolve(999); await flush();
    assert.equal(expired.loads, 1, 'an expired disk selection must display before its network refresh finishes');
    assert.equal(expired.calls, 2);
    assert.equal(expired.pending[0].params.refresh, true);
    const sameUpdate = expired.context.refreshHomeRecommendations(expired.context.session, {});
    assert.equal(expired.calls, 2, 're-entering home must share the active network refresh');
    expired.resolve(); await updating;
    await sameUpdate;
    assert.equal(expired.loads, 2, 'a successful refresh must replace the expired group');
    assert.equal([...expired.timers.values()][0].delay, 6 * 60 * 60 * 1000);

    const offline = harness();
    const refreshing = offline.load(); offline.resolve(999); await flush();
    offline.pending.shift().reject(new Error('offline')); await refreshing;
    assert.equal(offline.loads, 1, 'a failed refresh must preserve cached banner artwork');
    assert.equal(offline.timers.size, 0, 'failure must not start a retry loop');
    await offline.load(); assert.equal(offline.calls, 2); assert.equal(offline.loads, 2);
    assert.equal(offline.hero.children.length, 0, 'no error UI should be inserted');
    offline.context.session = {};
    const restart = offline.load(); assert.equal(offline.calls, 3, 'a new application session may update again');
    offline.resolve(); await restart;
    console.log('HOME_RECOMMENDATIONS_OK: expiration, cached-first display, background refresh, failure preservation, next-launch retry and navigation');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
