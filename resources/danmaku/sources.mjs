// 固定版本源适配器；播放器本地顺序执行，节目引用直接使用源站页面 URL。
import { Globals } from 'danmu-api-server/danmu_api/configs/globals.js';
import TencentSource from 'danmu-api-server/danmu_api/sources/tencent.js';
import IqiyiSource from 'danmu-api-server/danmu_api/sources/iqiyi.js';
import YoukuSource from './youku.mjs';
import { runWithHttpCache } from 'danmu-api-server/danmu_api/utils/http-util.js';
import { extractSeasonNumberFromAnimeTitle } from 'danmu-api-server/danmu_api/utils/common-util.js';
import { convertToDanmakuJson } from 'danmu-api-server/danmu_api/utils/danmu-util.js';
import { withSourceRiskGuard, SourceRiskControlError } from './risk.mjs';

const normalize = value => String(value || '').normalize('NFKC').replace(/\s+/g, ' ').trim().toLowerCase();

export function exactTitle(candidate, target) {
  // 别名与主标题属于同一季，不能因英文别名未带季号而绕过源站明确的季号。
  if (candidate.season != null && (!Number.isInteger(candidate.season) || candidate.season < 1 ||
      candidate.season !== target.season)) return false;
  const wanted = [target.title, target.original_title].filter(Boolean);
  const texts = [candidate.title, ...(candidate.aliases || [])].filter(Boolean);
  return texts.some(text => wanted.some(title => {
    // 作品名本身含数字时必须先比较完整名称，不能把“怪兽8号”等数字当季号。
    if (normalize(text) === normalize(title)) {
      const explicit = /第\s*([0-9一二三四五六七八九十]+)\s*季|(?:Season|\bS)\s*\d+/i.test(text);
      if (explicit) return extractSeasonNumberFromAnimeTitle(text).season === target.season;
      return candidate.season != null || target.season <= 1;
    }
    const parsed = extractSeasonNumberFromAnimeTitle(text);
    return parsed.season === target.season && normalize(parsed.baseTitle) === normalize(title);
  }));
}

export function youkuEpisodeNumber(ep, programTitle) {
  const stage = String(ep.stage || '').trim();
  const title = normalize(ep.title);
  if (!/^[1-9]\d*$/.test(stage) || !title.startsWith(normalize(programTitle)) || /预告|花絮|片花|抢先看|解说|特辑/.test(title)) return null;
  const suffix = title.slice(normalize(programTitle).length);
  // 优酷有明确 stage；标题中的集号必须一致，不用 seq 或数组位置推断。
  const match = /^\s+(?:第\s*)?(\d+)(?:\s*集)?(?=$|[\s:：·._-])/.exec(suffix);
  return match && Number(match[1]) === Number(stage) ? Number(stage) : null;
}

function youkuEpisodeUrl(ep) {
  if (!/^[A-Za-z0-9=_-]+$/.test(String(ep.id || ''))) throw new Error('youku: invalid episode video ID');
  const url = `https://v.youku.com/v_show/id_${ep.id}.html`;
  if (ep.link) {
    const parsed = new URL(ep.link);
    if (!['http:', 'https:'].includes(parsed.protocol) || parsed.hostname !== 'v.youku.com' || parsed.pathname !== new URL(url).pathname ||
        parsed.username || parsed.password || parsed.port) throw new Error('youku: episode ID and URL disagree');
  }
  return url;
}

export async function search(source, provider, target) {
  const manual = target.manual === true;
  const title = target.title;
  const query = manual ? target.query : target.season > 1 ? `${title} 第${target.season}季` : title;
  const programs = await source.search(query);
  const movie = !target.season && !target.episode;
  // 腾讯适配器保留 2D/3D 标签；它们仍是动漫，不采用任意前缀或模糊类型匹配。
  const candidates = programs.filter(program => manual ? ['电影', '电视剧', '动漫', '2D动漫', '3D动漫', '纪录片'].includes(program.type) : exactTitle(program, target) &&
    (movie ? program.type === '电影' && target.year > 0 && Number(program.year) === target.year
      : ['电视剧', '动漫', '2D动漫', '3D动漫', '纪录片'].includes(program.type)));
  const animes = [];
  for (const program of candidates) {
    const programMovie = manual ? program.type === '电影' : movie;
    const episodes = await source.getEpisodes(program.mediaId, provider === 'tencent' ? (program.chapterContexts || []) : manual ? null : target.season || null);
    const selected = episodes.filter(ep => programMovie ? (manual || episodes.length === 1) : provider === 'youku' ? youkuEpisodeNumber(ep, program.title) !== null :
      Number.isInteger(Number(provider === 'tencent' ? ep.title : ep.order)) && Number(provider === 'tencent' ? ep.title : ep.order) > 0);
    const entries = [];
    for (const ep of selected) {
      const url = provider === 'tencent' ? `https://v.qq.com/x/cover/${program.mediaId}/${ep.vid}.html` : provider === 'youku' ? youkuEpisodeUrl(ep) : ep.link;
      const episodeTitle = provider === 'tencent' ? ep.unionTitle : ep.title;
      // 多季合辑中不能只按数组下标选集；明确带作品名的分集必须再次核对作品与季。
      if (!manual && !movie && provider !== 'youku') {
        const remainder = normalize(episodeTitle).slice(normalize(program.title).length);
        if (!normalize(episodeTitle).startsWith(normalize(program.title)) || /^\d/.test(remainder)) continue;
      }
      if (!url) throw new Error(`${provider}: selected episode has no URL`);
      entries.push({ episodeUrl: url, episodeTitle,
        episodeNumber: programMovie ? '' : String(provider === 'youku' ? youkuEpisodeNumber(ep, program.title) : Number(provider === 'tencent' ? ep.title : ep.order)), url });
    }
    if (entries.length) animes.push({ animeId: program.mediaId, animeTitle: program.title, type: programMovie ? 'movie' : 'tv', typeDescription: program.type, episodes: entries });
  }
  return animes;
}

export function safeError(message) {
  return String(message).replace(/https?:\/\/[^\s"'<>]+/g, value => {
    try { const url = new URL(value); return url.origin + url.pathname; } catch { return '[URL]'; }
  }).replace(/(_m_h5_tk(?:_enc)?|cookie|token|sign)(\s*[:=]\s*)[^\s;,"}]+/gi, '$1$2[redacted]').slice(0, 1500);
}

export function assertSourceSuccess(provider) {
  const errors = Globals.logBuffer.filter(entry => entry.level === 'error' ||
    /响应为空|重试后仍失败/.test(entry.message));
  if (errors.length) throw new Error(safeError(`${provider}: ${errors.map(entry => entry.message).join('; ')}`));
}

export async function comments(source, provider, url, concurrency = 6) {
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 8) throw new Error('invalid segment concurrency');
  const segments = await source.getEpisodeDanmuSegments(url);
  assertSourceSuccess(provider);
  if (!segments || !Array.isArray(segments.segmentList)) throw new Error(`${provider}: invalid segment response`);
  const raw = [];
  let next = 0;
  // 只抓当前命中源；并发有界，任何分片错误都不保存部分成功结果。
  let failure;
  await Promise.all(Array.from({ length: Math.min(concurrency, segments.segmentList.length) }, async () => {
    while (!failure && next < segments.segmentList.length) {
      try {
        const batch = await source.getEpisodeSegmentDanmu(segments.segmentList[next++]);
        assertSourceSuccess(provider);
        if (!Array.isArray(batch)) throw new Error(`${provider}: invalid segment comments`);
        raw.push(...batch);
      } catch (error) { failure ??= error; }
    }
  }));
  if (failure) throw failure;
  return convertToDanmakuJson(source.formatComments(raw), provider === 'tencent' ? 'qq' : provider === 'iqiyi' ? 'qiyi' : 'youku');
}

async function runSource(action, provider, data) {
  if (!['tencent', 'iqiyi', 'youku'].includes(provider)) throw new Error('unsupported native source');
  Globals.logBuffer = [];
  Globals.init({ SOURCE_ORDER: provider, LOG_LEVEL: 'info', VOD_REQUEST_TIMEOUT: '15000', STRICT_TITLE_MATCH: 'true',
    LOCAL_CACHE_ENABLED: 'false', DANMU_LIMIT: '0', GROUP_MINUTE: '0', LIKE_SWITCH: 'false' });
  const source = provider === 'tencent' ? new TencentSource() : provider === 'iqiyi' ? new IqiyiSource() : new YoukuSource();
  let result;
  if (action === 'search') result = await search(source, provider, data);
  else if (action === 'comment') {
    const parsed = new URL(data.url);
    const allowed = provider === 'tencent' ? parsed.hostname === 'v.qq.com' && /^\/x\/cover\/[a-z0-9]+\/[a-z0-9]+\.html$/.test(parsed.pathname)
      : provider === 'iqiyi' ? parsed.hostname === 'www.iqiyi.com' && /^\/v_[a-z0-9]+\.html$/.test(parsed.pathname)
      : parsed.hostname === 'v.youku.com' && /^\/v_show\/id_[A-Za-z0-9=_-]+\.html$/.test(parsed.pathname);
    if (parsed.protocol !== 'https:' || !allowed || parsed.username || parsed.password || parsed.port || parsed.search || parsed.hash) throw new Error('invalid source episode URL');
    result = { comments: await comments(source, provider, data.url, data.segment_concurrency ?? 6) };
  } else throw new Error('unknown bridge action');
  // 上游源库可能记录错误后返回 []，必须把这种失败显式带回播放器，不能缓存为空。
  assertSourceSuccess(provider);
  return result;
}
export async function run(action, provider, data) {
  return withSourceRiskGuard(provider, () => runSource(action, provider, data));
}

export function sourceErrorEnvelope(error) {
  const result = { ok: false, error: safeError(error.message) };
  if (error instanceof SourceRiskControlError) {
    result.code = error.code;
    result.risk_reason = error.riskReason;
  }
  return result;
}
