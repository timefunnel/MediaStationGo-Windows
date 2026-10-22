// Exercise the actual renderer handlers without an account, network or native player.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const elements = new Map();
const byId = id => {
    if (!elements.has(id)) elements.set(id, { _value: '', disabled: true, options: [],
        get value() { return this._value; }, set value(value) { this._value = String(value); },
        replaceChildren(...children) { this.options = children; this.value = children[0]?.value || ''; },
        add(option) { this.options.push(option); },
        addEventListener(name, handler) { this[name] = handler; } });
    return elements.get(id);
};
const programs = [0, 1].map(index => ({ source: 'tencent', animeTitle: `候选节目${index}`, type: 'tv',
    episodes: [{ episodeUrl: `https://v.qq.com/x/cover/test/episode${index}.html`, episodeTitle: '第二集', episodeNumber: 2 }] }));
const calls = [], feedback = [], storage = new Map(), syncCalls = [];
let persistence = 'server_extension';
const context = vm.createContext({ byId, player: { card: { id: 'item' }, positionMs: 0 },
    Option: function (text, value) { this.text = text; this.value = String(value); },
    danmakuReferenceContext: null, danmakuPrograms: [], danmakuReferenceEditing: false, danmakuSearchRequest: 0,
    danmakuRenderer: { clear() {}, load() {} }, playerPlaybackRate: 1,
    danmakuSyncEnabled: true, danmakuSyncSettingKey: 'sync',
    window: { localStorage: { setItem: (key, value) => storage.set(key, value) }, jmpNative: { playerSetDanmakuSync: value => syncCalls.push(value) } },
    showPlayerFeedback: text => feedback.push(text), friendlyError: error => error.message,
    nativeRequest: async (...args) => { calls.push(args); return args[1] === 'danmaku_candidates' ? { programs } : { persistence }; } });
for (const name of ['selectedDanmakuReference', 'refreshDanmakuEpisodes', 'showDanmakuPrograms',
    'updateDanmakuReference', 'persistDanmakuReference', 'handleDanmakuEvent', 'pushDanmakuSync']) {
    const pattern = new RegExp(`^    (?:async )?function ${name}\\(`, 'm');
    const start = source.search(pattern);
    const next = source.slice(start + 1).search(/^    (?:async )?function /m);
    assert.ok(start >= 0 && next >= 0, name);
    vm.runInContext(source.slice(start, start + 1 + next), context);
}
const start = source.indexOf("    byId('player-danmaku-sync').addEventListener(");
const end = source.indexOf('    const playerVolumeControl', start);
assert.ok(start >= 0 && end > start);
vm.runInContext(source.slice(start, end), context);
const click = id => byId(id).click({ currentTarget: byId(id) });

(async () => {
    context.handleDanmakuEvent({ kind: 'context', mediaId: 'item', playSessionId: 'play-1', taskId: '8', revision: '3',
        target: { title: '当前节目', episode: 2 }, association: { source: 'iqiyi',
            episode_url: 'https://www.iqiyi.com/v_test.html', anime_title: '当前节目', episode_title: '第二集', offset_seconds: 2 } });
    assert.equal(byId('player-danmaku-offset-save').disabled, false);
    byId('player-danmaku-offset').value = '-2.5';
    await click('player-danmaku-offset-save');
    assert.equal(calls.length, 1, 'offset saving must not search');
    assert.equal(calls[0][0], 'playerSetDanmakuReference');
    assert.deepEqual(JSON.parse(calls[0][2][0]), { media_id: 'item', play_session_id: 'play-1', task_id: '8',
        source: 'iqiyi', episode_url: 'https://www.iqiyi.com/v_test.html', anime_title: '当前节目', episode_title: '第二集',
        offset_seconds: -2.5, expected_revision: '3' });

    byId('player-danmaku-source').value = 'tencent';
    byId('player-danmaku-source').change();
    assert.equal(byId('player-danmaku-save').disabled, true);
    assert.equal(byId('player-danmaku-offset-save').disabled, false);
    await click('player-danmaku-offset-save');
    assert.equal(calls.length, 2);
    assert.equal(JSON.parse(calls[1][2][0]).source, 'iqiyi', 'search draft must not change current offset target');
    byId('player-danmaku-offset').value = '601';
    await click('player-danmaku-offset-save');
    assert.equal(calls.length, 2, 'invalid offset must not reach native IPC');

    byId('player-danmaku-query').value = '新节目';
    await click('player-danmaku-search');
    assert.equal(calls[2][0], 'playerSearchDanmaku');
    assert.equal(byId('player-danmaku-program').value, '', 'ambiguous programs require an explicit choice');
    byId('player-danmaku-program').value = '1';
    byId('player-danmaku-program').change();
    assert.equal(byId('player-danmaku-episode').value, '0', 'episode selection uses the actual episode number');
    await click('player-danmaku-save');
    const selected = JSON.parse(calls[3][2][0]);
    assert.equal(selected.episode_url, programs[1].episodes[0].episodeUrl);
    assert.equal(selected.offset_seconds, 0, 'a different episode starts with no offset');

    byId('player-danmaku-sync').checked = false;
    byId('player-danmaku-sync').change({ currentTarget: byId('player-danmaku-sync') });
    assert.equal(storage.get('sync'), 'false');
    assert.deepEqual(syncCalls, [false]);
    persistence = 'local_only';
    context.handleDanmakuEvent({ kind: 'load', mediaId: 'item', playSessionId: 'play-1', taskId: '9', revision: '1',
        persistence, notice: '跨设备同步已关闭，选源和偏移仅保存在本机', association: { source: 'iqiyi',
            episode_url: 'https://www.iqiyi.com/v_test.html', offset_seconds: 0 } });
    assert.ok(byId('player-danmaku-sync-status').textContent.includes('仅保存在本机'));
    await click('player-danmaku-offset-save');
    assert.equal(feedback.at(-1), '弹幕偏移已保存至本机', 'local changes must never claim to be synced');

    let finishSearch;
    context.nativeRequest = () => new Promise(resolve => { finishSearch = resolve; });
    const pending = click('player-danmaku-search');
    context.handleDanmakuEvent({ kind: 'clear' });
    finishSearch({ programs });
    await pending;
    assert.equal(byId('player-danmaku-program').disabled, true, 'a late search cannot populate another playback');
    assert.equal(byId('player-danmaku-offset-save').disabled, true);
    assert.equal(byId('player-danmaku-save').disabled, true);
    context.handleDanmakuEvent({ kind: 'context', mediaId: 'other-item', revision: '99' });
    assert.equal(context.danmakuReferenceContext, null, 'another item cannot populate this form');
    assert.ok(!source.includes('player-danmaku-url'));
    const html = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.html'), 'utf8');
    assert.ok(!html.includes('本集节目地址'));
    console.log('LOCAL_DANMAKU_UI_OK');
})().catch(error => { console.error(error); process.exitCode = 1; });
