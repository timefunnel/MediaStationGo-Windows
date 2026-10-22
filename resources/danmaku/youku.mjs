// 复用固定版本源的搜索、视频信息和签名，仅收紧分集/分页/错误校验。
import YoukuSource from 'danmu-api-server/danmu_api/sources/youku.js';
import { Globals } from 'danmu-api-server/danmu_api/configs/globals.js';
import { httpPost, buildQueryString } from 'danmu-api-server/danmu_api/utils/http-util.js';
import { getExplicitSeasonNumber } from 'danmu-api-server/danmu_api/utils/common-util.js';

function explicitTitleSeason(title) {
  // 只拆明确的尾部季号，保留作品名中的数字、空格和标点。
  const marker = /(?:第\s*[0-9一二三四五六七八九十]+\s*季|\s+(?:Season|S)\s*\d+)\s*$/i.exec(title);
  if (!marker) return { baseTitle: title, season: null };
  const season = getExplicitSeasonNumber(marker[0]);
  const baseTitle = title.slice(0, marker.index).trim();
  if (!baseTitle || !Number.isInteger(season) || season < 1) throw new Error('youku: invalid explicit title season');
  return { baseTitle, season };
}

export function parseYoukuTitle(value) {
  const raw = String(value || '').trim();
  // 仅提取源站明示的完整尾注，不把英文标题内的逗号当作别名分隔符。
  const annotation = /^(.*?)\s*(?:\(\s*别名\s*[:：]\s*([^()（）]+)\)|（\s*别名\s*[:：]\s*([^()（）]+)）)\s*$/.exec(raw);
  const title = annotation ? annotation[1].trim() : raw;
  const alias = annotation ? (annotation[2] || annotation[3]).trim() : '';
  if (annotation && (!title || !alias)) throw new Error('youku: invalid explicit title alias');
  const canonical = explicitTitleSeason(title);
  const alternate = alias ? explicitTitleSeason(alias) : null;
  const seasons = new Set([canonical.season, alternate?.season].filter(season => season != null));
  if (seasons.size > 1) throw new Error('youku: canonical and alias season conflict');
  const aliases = [...new Set([
    ...(canonical.season != null ? [canonical.baseTitle] : []),
    ...(alias ? [alias] : []),
    ...(alternate?.season != null ? [alternate.baseTitle] : []),
  ])].filter(text => text !== title);
  return { title, aliases, season: [...seasons][0] ?? null };
}

export function parseYoukuSegment(payload) {
  if (!Array.isArray(payload?.ret) || !payload.ret.length || !payload.ret.every(code => String(code).startsWith('SUCCESS'))) {
    const codes = Array.isArray(payload?.ret) ? payload.ret.map(value => /^[A-Z][A-Z0-9_]*/.exec(String(value))?.[0] || 'invalid').join(',') : 'missing ret';
    throw new Error(`youku: segment API failed (${codes})`);
  }
  let result;
  try { result = JSON.parse(payload.data?.result); }
  catch { throw new Error('youku: invalid segment result JSON'); }
  if (String(result?.code) !== '1' || !Array.isArray(result.data?.result)) {
    const code = Number.isSafeInteger(Number(result?.code)) ? Number(result.code) : 'invalid';
    throw new Error(`youku: segment result failed or is incomplete (code=${code})`);
  }
  return result.data.result;
}

export default class StrictYoukuSource extends YoukuSource {
  constructor(post = httpPost) {
    super();
    this.post = post;
  }

  _getFallbackCna() {
    throw new Error('youku: anonymous client identifier unavailable; local fallback is disabled');
  }

  filterYoukuSearchItem(component, keyword) {
    const program = super.filterYoukuSearchItem(component, keyword);
    return program ? { ...program, ...parseYoukuTitle(program.title) } : null;
  }

  async search(keyword) {
    const programs = await super.search(keyword);
    if (Globals.logBuffer.some(entry => entry.message.includes('[youku] 搜索无结果'))) {
      throw new Error('youku: search response is missing pageComponentList');
    }
    return programs;
  }

  async getEpisodes(showId) {
    const videos = [];
    let total;
    for (let page = 1; total === undefined || videos.length < total; page++) {
      const response = await this._getEpisodesPage(showId, page, 100);
      const count = Number(response?.total);
      if (!response || !Array.isArray(response.videos) || !Number.isSafeInteger(count) || count < 0 || count > 10000 ||
          (total !== undefined && count !== total) || (count > videos.length && !response.videos.length)) {
        throw new Error('youku: invalid or incomplete episode page');
      }
      total = count;
      videos.push(...response.videos);
      if (videos.length > total || page > 100) throw new Error('youku: invalid episode count');
    }
    if (new Set(videos.map(ep => ep.id)).size !== videos.length) throw new Error('youku: duplicate episode pages');
    return videos;
  }

  async getEpisodeSegmentDanmu(segment) {
    const response = await this.post(segment.url, buildQueryString({ data: segment.data }), {
      headers: {
        Cookie: `_m_h5_tk=${segment._m_h5_tk};_m_h5_tk_enc=${segment._m_h5_tk_enc};`,
        Referer: 'https://v.youku.com', 'Content-Type': 'application/x-www-form-urlencoded',
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/79.0.3945.88 Safari/537.36',
      }, allow_redirects: false, retries: 1,
    });
    return parseYoukuSegment(response?.data);
  }
}
