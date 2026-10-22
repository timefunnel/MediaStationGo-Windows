import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DiskCache, cleanComments, loadDanmaku, searchCandidates, validateReference } from './local.mjs';

const url = 'https://www.iqiyi.com/v_test.html';
const ref = { source: 'iqiyi', episode_url: url, offset_seconds: 2 };
const target = { title: '测试', tmdb_id: '1', season: 1, episode: 2, year: 2020 };
const program = { animeTitle: '测试', episodes: [{ episodeUrl: url, episodeNumber: '2', episodeTitle: '测试 2' }] };
const raw = { comments: [{ cid: '9007199254740993', p: '1,1,16777215,u', m: '弹幕' }] };
async function withCache(work) {
  const dir = await mkdtemp(join(tmpdir(), 'jfn-local-danmaku-'));
  let now = 1000;
  try { await work(new DiskCache(dir, () => now), amount => { now += amount; }); }
  finally { await rm(dir, { recursive: true, force: true }); }
}

test('shared source URL loads directly without a search or numeric reference registry', () => withCache(async cache => {
  const calls = [];
  const run = async (...args) => { calls.push(args); return raw; };
  const first = await loadDanmaku({ target, association: ref }, cache, run);
  const next = await loadDanmaku({ target, association: { ...ref, offset_seconds: 5 } }, cache, run);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], 'comment');
  assert.equal(calls[0][2].url, url);
  assert.equal(first.comments[0].time, 3);
  assert.equal(next.comments[0].time, 6);
  assert.equal(first.comments[0].cid, '9007199254740993');
}));

test('search cache covers an entire season and preserves source priority', () => withCache(async cache => {
  const searches = [];
  const programs = { ...program, episodes: [...program.episodes, { episodeUrl: 'https://www.iqiyi.com/v_next.html', episodeNumber: '3' }] };
  const run = async (action, source, data) => {
    if (action === 'comment') return raw;
    searches.push([source, data]);
    return source === 'iqiyi' ? [programs] : [];
  };
  const first = await loadDanmaku({ target }, cache, run);
  const next = await loadDanmaku({ target: { ...target, episode: 3 } }, cache, run);
  assert.equal(first.association.episode_url, url);
  assert.equal(next.association.episode_url, 'https://www.iqiyi.com/v_next.html');
  assert.deepEqual(searches.map(item => item[0]), ['tencent', 'iqiyi']);
  assert.equal('episode' in searches[0][1], false);
}));

test('selected source failure is never replaced with another source or cached as empty', () => withCache(async cache => {
  let calls = 0;
  const run = async () => { calls++; throw new Error('分片缺失'); };
  await assert.rejects(loadDanmaku({ association: ref }, cache, run), /分片缺失/);
  await assert.rejects(loadDanmaku({ association: ref }, cache, run), /分片缺失/);
  assert.equal(calls, 2);
}));

test('risk cooldown persists across cache instances and expires after thirty minutes', () => withCache(async (cache, advance) => {
  let calls = 0;
  const run = async () => { calls++; throw Object.assign(new Error('风控'), { code: 'danmaku_source_risk_control' }); };
  await assert.rejects(loadDanmaku({ association: ref }, cache, run), /风控/);
  const other = new DiskCache(cache.directory, cache.clock);
  await assert.rejects(loadDanmaku({ association: ref }, other, run), /冷却中/);
  assert.equal(calls, 1);
  advance(1800000);
  await assert.rejects(loadDanmaku({ association: ref }, other, run), /风控/);
  assert.equal(calls, 2);
}));

test('unknown metadata and ambiguous candidates do not guess an episode', () => withCache(async cache => {
  assert.equal((await loadDanmaku({ target: {} }, cache, () => assert.fail())).status, 'unmatched');
  assert.equal((await loadDanmaku({ target: { ...target, tmdb_id: '0' } }, cache, () => assert.fail())).status, 'unmatched');
  const duplicate = { ...program, episodes: [...program.episodes, { ...program.episodes[0], episodeUrl: 'https://www.iqiyi.com/v_other.html' }] };
  const result = await loadDanmaku({ target }, cache, async (action, source) => {
    assert.equal(action, 'search');
    return source === 'iqiyi' ? [duplicate] : [];
  });
  assert.equal(result.status, 'unmatched');
}));

test('references reject mismatched hosts, private URLs, signed links and out-of-range offsets', () => {
  for (const episode_url of ['http://127.0.0.1/a', 'https://www.iqiyi.com.evil/v_test.html',
    'https://www.iqiyi.com:443/v_test.html', 'https://www.iqiyi.com/%76_test.html', url + '#', url + '?token=secret'])
    assert.throws(() => validateReference({ ...ref, episode_url }));
  assert.throws(() => validateReference({ ...ref, offset_seconds: 601 }));
});

test('manual search returns programs and episodes for selection without fetching comments', () => withCache(async cache => {
  const calls = [];
  const run = async (...args) => { calls.push(args); return [program]; };
  const first = await searchCandidates({ source: 'iqiyi', query: '测试' }, cache, run);
  const next = await searchCandidates({ source: 'iqiyi', query: '测试' }, cache, run);
  assert.equal(first.status, 'candidates');
  assert.equal(first.programs[0].source, 'iqiyi');
  assert.equal(first.programs[0].episodes[0].episodeTitle, '测试 2');
  assert.equal(next.programs[0].episodes[0].episodeUrl, url);
  assert.deepEqual(calls, [['search', 'iqiyi', { manual: true, query: '测试' }]]);
  await assert.rejects(searchCandidates({ source: 'iqiyi', query: ' ' }, cache, run), /节目名称/);
}));

test('cleaning reports malformed entries, duplicates, unsupported modes and samples across the timeline', () => {
  const result = cleanComments([...raw.comments, ...raw.comments, { p: '2,8,1,u', m: '代码' },
    { p: 'NaN,1,1,u', m: '错误' }, { p: '100,5,1,u', m: '尾部' }], 0, 1);
  assert.equal(result.filtered, 2);
  assert.equal(result.skipped, 1);
  assert.equal(result.total, 2);
  assert.equal(result.comments.length, 1);
  assert.equal(result.truncated, true);
});
