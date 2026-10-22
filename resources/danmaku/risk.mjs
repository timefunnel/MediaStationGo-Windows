// 单次源请求内的风控保护；三十分钟冷却由播放器本地磁盘缓存持久化。
export class SourceRiskControlError extends Error {
  constructor(reason) {
    super(`danmaku_source_risk_control: ${reason}`);
    this.code = 'danmaku_source_risk_control';
    this.riskReason = reason;
  }
}

const explicitRisk = /风控|验证码|(?:访问|请求)过于频繁|请求频繁|频率限制|too many requests|rate[ _-]?limit(?:ed| exceeded)?|captcha|USER_VALIDATE|RGV587_ERROR/i;

export function responseRiskReason(provider, url, payload) {
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) return null;
  const parsed = new URL(url);
  if (provider === 'iqiyi' && parsed.hostname === 'mesh.if.iqiyi.com' &&
      parsed.pathname === '/portal/lw/search/homePageV3' && String(payload.code) === '-1') {
    return 'iqiyi_search_risk_control';
  }
  // 只检查明确失败的业务状态与错误字段，不扫描作品名或弹幕正文。
  const ret = Array.isArray(payload.ret) ? payload.ret : [payload.ret];
  if (ret.some(value => typeof value === 'string' && !value.startsWith('SUCCESS') && explicitRisk.test(value))) {
    return 'explicit_risk_control';
  }
  const failed = ['ret', 'code', 'status_code'].some(key =>
    payload[key] != null && !Array.isArray(payload[key]) && !['0', '200', 'SUCCESS', 'A00000'].includes(String(payload[key])));
  if (failed && ['msg', 'message', 'error_msg', 'errorMessage'].some(key =>
    typeof payload[key] === 'string' && explicitRisk.test(payload[key]))) return 'explicit_risk_control';
  return null;
}

export async function withSourceRiskGuard(provider, operation) {
  // 固定适配器在 Node >= 20.19 使用原生 fetch；生产镜像及本地约定为 Node >= 22。
  if (Number(process.versions.node.split('.')[0]) < 22) throw new Error('source risk guard requires Node.js 22 or later');
  const originalFetch = globalThis.fetch;
  const controller = new AbortController();
  let risk;
  const rejectRisk = reason => {
    risk ??= new SourceRiskControlError(reason);
    controller.abort(risk);
    throw risk;
  };
  globalThis.fetch = async (input, options = {}) => {
    if (risk) throw risk; // 源库捕获错误后即使尝试重试，也不能再发出 HTTP 请求。
    const signals = [controller.signal, options.signal || input?.signal].filter(Boolean);
    const response = await originalFetch(input, { ...options, signal: AbortSignal.any(signals) });
    if (risk) throw risk;
    if ([403, 429].includes(response.status)) rejectRisk(`http_${response.status}`);
    const url = String(input?.url || input);
    const text = response.text.bind(response);
    response.text = async () => {
      const body = await text();
      if (risk) throw risk;
      // 不额外读取或复制响应体；二进制分片只检查 HTTP 状态。
      if (body.trimStart().startsWith('{')) {
        let payload;
        try { payload = JSON.parse(body); } catch { return body; } // 保留原适配器的格式错误处理。
        const reason = responseRiskReason(provider, url, payload);
        if (reason) rejectRisk(reason);
      }
      return body;
    };
    return response;
  };
  try {
    const result = await operation();
    if (risk) throw risk; // 第三方 catch 后返回 [] 也必须保留原始风控失败。
    return result;
  } catch (error) {
    throw risk || error;
  } finally {
    globalThis.fetch = originalFetch;
  }
}
