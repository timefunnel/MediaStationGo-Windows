import { createHash } from 'node:crypto';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

export const providers = ['tencent', 'iqiyi', 'youku'];

export function validateReference(reference) {
  if (!reference || !providers.includes(reference.source)) throw new Error('不支持的弹幕源');
  const url = new URL(reference.episode_url);
  const valid = reference.source === 'tencent'
    ? url.hostname === 'v.qq.com' && /^\/x\/cover\/[a-z0-9]+\/[a-z0-9]+\.html$/.test(url.pathname)
    : reference.source === 'iqiyi'
      ? url.hostname === 'www.iqiyi.com' && /^\/v_[a-z0-9]+\.html$/.test(url.pathname)
      : url.hostname === 'v.youku.com' && /^\/v_show\/id_[A-Za-z0-9=_-]+\.html$/.test(url.pathname);
  if (url.protocol !== 'https:' || !valid || url.username || url.password || url.port || url.href !== reference.episode_url || /[?#]/.test(reference.episode_url))
    throw new Error('弹幕节目地址与源不匹配');
  const offset = reference.offset_seconds ?? 0;
  if (!Number.isFinite(offset) || Math.abs(offset) > 600) throw new Error('弹幕偏移必须在 -600 至 600 秒之间');
  return reference;
}

export class DiskCache {
  constructor(directory, clock = Date.now) { this.directory = directory; this.clock = clock; }
  path(key) { return join(this.directory, createHash('sha256').update(JSON.stringify(key)).digest('hex') + '.json'); }
  async get(key, ttl) {
    let text;
    try { text = await readFile(this.path(key), 'utf8'); }
    catch (error) { if (error.code === 'ENOENT') return null; throw error; }
    const entry = JSON.parse(text);
    if (!Number.isFinite(entry.saved_at) || !('value' in entry)) throw new Error('本地弹幕缓存格式无效');
    return this.clock() - entry.saved_at < ttl ? entry.value : null;
  }
  async set(key, value) {
    await mkdir(this.directory, { recursive: true });
    const path = this.path(key), temp = path + `.${process.pid}.tmp`;
    await writeFile(temp, JSON.stringify({ saved_at: this.clock(), value }), 'utf8');
    await rename(temp, path);
  }
}

export function cleanComments(entries, offset = 0, limit = 6000) {
  if (!Array.isArray(entries)) throw new Error('弹幕源返回的内容格式无效');
  const comments = [], seen = new Set();
  let skipped = 0, filtered = 0;
  for (const entry of entries) {
    const p = String(entry?.p ?? entry?.c ?? '').split(',');
    const time = Number(p[0]), mode = Number(p[1]);
    const color = Number(p.length >= 8 ? p[3] : p[2]);
    const text = typeof entry?.m === 'string' ? entry.m.trim().replace(/\r\n?/g, '\n') : '';
    if (p.length < 4 || !p[0].trim() || !Number.isFinite(time) || time < 0 ||
        !Number.isInteger(mode) || !Number.isInteger(color) || color < 0 || color > 0xffffff || !text) {
      skipped++; continue;
    }
    const key = JSON.stringify([Math.round(time * 100), mode, text]);
    if (![1, 4, 5, 6].includes(mode) || seen.has(key)) { filtered++; continue; }
    seen.add(key);
    comments.push({ cid: String(entry.cid ?? ''), time: Math.max(0, time + offset),
      mode: mode === 6 ? 1 : mode, color, text });
  }
  comments.sort((a, b) => a.time - b.time || a.cid.localeCompare(b.cid));
  const total = comments.length;
  return { comments: total > limit ? Array.from({ length: limit }, (_, i) => comments[Math.floor(i * total / limit)]) : comments,
    total, filtered, skipped, truncated: total > limit };
}

function sourceCache(cache, runSource) {
  return async (source, action, data) => {
    const key = ['local-v1', source, action, data];
    const ttl = action === 'search' ? 86400000 : 604800000;
    const cached = await cache.get(key, ttl);
    if (cached !== null) {
      const empty = Array.isArray(cached) ? cached.length === 0 : cached.comments?.length === 0;
      if (!empty || await cache.get(key, 3600000) !== null) return cached;
    }
    const riskKey = ['risk', source];
    const risk = await cache.get(riskKey, 1800000);
    if (risk) throw new Error(`${source} 弹幕源风控冷却中，请稍后重试`);
    try {
      const value = await runSource(action, source, data);
      await cache.set(key, value);
      return value;
    } catch (error) {
      if (error.code === 'danmaku_source_risk_control') await cache.set(riskKey, { reason: error.riskReason });
      throw error;
    }
  };
}

// runSource executes one source at a time because the upstream adapters have process-global state.
// A failed selected source is an error; it never causes an automatic source change.
export async function loadDanmaku(input, cache, runSource) {
  const target = input.target;
  let association = input.association;
  const cachedCall = sourceCache(cache, runSource);
  if (association) validateReference(association);
  else {
    if (!target || !/^[1-9]\d*$/.test(String(target.tmdb_id)) || !target.title || !Number.isInteger(target.season) || !Number.isInteger(target.episode) ||
        !(target.season > 0 && target.episode > 0 || target.season === 0 && target.episode === 0 && target.year > 0)) {
      return { status: 'unmatched', message: '缺少准确的作品、季集或年份信息，无法自动匹配弹幕' };
    }
    const errors = [], attempts = [];
    for (const source of providers) {
      let programs;
      const { episode: ignored, ...seasonTarget } = target;
      try { programs = await cachedCall(source, 'search', seasonTarget); }
      catch (error) { errors.push(`${source}: ${error.message}`); continue; }
      const candidates = new Map();
      for (const program of programs) for (const ep of program.episodes) {
        if (target.episode && Number(ep.episodeNumber) !== target.episode) continue;
        const ref = { source, episode_url: ep.episodeUrl, anime_title: program.animeTitle,
          episode_title: ep.episodeTitle, offset_seconds: 0 };
        validateReference(ref);
        candidates.set(ep.episodeUrl, ref);
      }
      attempts.push(`${source}: ${candidates.size} 个候选`);
      if (candidates.size === 1) { association = [...candidates.values()][0]; break; }
    }
    if (!association) {
      if (errors.length) throw new Error(errors.join('；'));
      return { status: 'unmatched', message: `未找到唯一准确的弹幕节目（${attempts.join('；')}）` };
    }
  }
  const raw = await cachedCall(association.source, 'comment', { url: association.episode_url, segment_concurrency: 6 });
  return { status: 'matched', association, ...cleanComments(raw.comments, association.offset_seconds) };
}

// Manual correction lists source programs and their actual episodes; it never saves a choice.
export async function searchCandidates(input, cache, runSource) {
  if (!providers.includes(input.source)) throw new Error('请选择弹幕源');
  const query = String(input.query ?? '').trim();
  if (!query || query.length > 100) throw new Error('节目名称需要 1 至 100 个字');
  const programs = await sourceCache(cache, runSource)(input.source, 'search', { manual: true, query });
  for (const program of programs) for (const ep of program.episodes)
    validateReference({ source: input.source, episode_url: ep.episodeUrl });
  return { status: 'candidates', programs: programs.map(program => ({ ...program, source: input.source })) };
}
