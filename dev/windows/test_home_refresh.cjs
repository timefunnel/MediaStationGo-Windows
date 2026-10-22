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
    assert.ok(next, `function boundary after ${name}`);
    return source.slice(match.index, match.index + match[0].length + next.index);
}
function harness(response) {
    let timer, renders = 0, restores = 0;
    const previous = { libraries: [], resume: [{ id: 'one', resumePositionMs: 12 }], latest: [], latestSections: [], cache: { status: 'hit', savedAtMs: 1 } };
    const context = vm.createContext({
        homeRefreshTimer: 0, homeRefreshGeneration: 0, homeRefreshFailures: 0, homeRefreshDelayMs: 1200,
        session: {}, homeData: previous, currentView: { kind: 'home', data: previous },
        window: { clearTimeout() {}, setTimeout(fn) { timer = fn; return 1; } }, console,
        nativeRequest: async () => response,
        renderHome() { renders++; }, captureView: () => ({ scrollTop: 650, focusKey: 'media:one' }),
        restoreViewState(saved, options) { assert.equal(saved.scrollTop,650);assert.equal(options.restoreFocus,true);restores++; },
    });
    for (const name of ['homeContentChanged', 'scheduleHomeRefresh']) vm.runInContext(handler(name), context);
    return { context, previous, run: async () => { context.scheduleHomeRefresh(); await timer(); }, counts: () => ({ renders, restores }) };
}
async function main() {
    const fresh = { libraries: [], resume: [{ id: 'one', resumePositionMs: 12 }], latest: [], latestSections: [], cache: { status: 'refreshed', savedAtMs: 2 } };
    const same = harness(fresh);
    await same.run();
    assert.deepEqual(same.counts(), { renders: 0, restores: 0 });
    assert.equal(same.context.homeData, fresh);
    assert.equal(same.context.currentView.data, fresh, 'authoritative response must still update view data');
    assert.equal(same.context.homeContentChanged(null, fresh), true);
    for (const change of [
        { resume: [{ id: 'one', resumePositionMs: 20 }] },
        { latest: [{ id: 'new' }] },
        { libraries: [{ id: 'library', title: 'renamed' }] },
        { latestSections: [{ library: { id: 'library' }, items: [{ id: 'new' }] }] },
    ]) {
        const changed = harness({ ...fresh, ...change });
        await changed.run();
        assert.deepEqual(changed.counts(), { renders: 1, restores: 1 });
    }
    const away = harness({ ...fresh, latest: [{ id: 'new' }] });
    away.context.currentView = { kind: 'library' };
    await away.run();
    assert.deepEqual(away.counts(), { renders: 0, restores: 0 });

    const events = [];
    const node = () => ({ children: [], dataset: {}, append(...children) { this.children.push(...children); }, setAttribute() {}, get childElementCount() { return this.children.length; } });
    const render = vm.createContext({
        imageWidthFor() { events.push('measure');return 1440; },
        stopHeroCarousel() {}, replaceContent() { events.push('replace'); }, heroRevision: 1,
        maximumContinueWatchingItems:20, element:node, createSection:() => null, content:node(), heroCandidates:() => [],
        resumePlayback() {}, openDetail() {},
        loadHeroCandidates(hero, candidates, revision, width) { assert.equal(width,1440);events.push('artwork'); },
    });
    vm.runInContext(handler('renderHome'), render);
    render.renderHome({ resume: [], latest: [], latestSections: [] });
    assert.deepEqual(events, ['measure','replace','artwork'], 'new DOM must not force the artwork width measurement');

    const widths = [];
    const hero = { isConnected:true, dataset:{} };
    const candidates = vm.createContext({
        heroRevision:1, heroCarouselMaxCards:4,
        loadHeroArtwork:async (hero,card,width) => { widths.push(width);return {}; },
        heroArtworkFits:() => false, refreshHeroArtworkQualification() {}, console, friendlyError:String,
    });
    vm.runInContext(handler('loadHeroCandidates'), candidates);
    await candidates.loadHeroCandidates(hero, [{id:'one'},{id:'two'}],1,1440);
    assert.deepEqual(widths,[1440,undefined], 'only initial artwork should use the pre-mutation width; later requests remeasure after resize');
    console.log('HOME_REFRESH_OK: unchanged data preserves DOM; all content changes refresh; initial artwork measures before DOM replacement');
}
main().catch(error => { console.error(error);process.exitCode=1; });
