param(
    [int]$Port = 4173
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$WebRoot = Join-Path $RepoRoot 'src\web'

$PreviewShim = @'
(function () {
    'use strict';

    const svg = (title, start, end) => {
        const markup = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 540">
          <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop stop-color="${start}"/><stop offset="1" stop-color="${end}"/></linearGradient></defs>
          <rect width="960" height="540" fill="url(#g)"/>
          <circle cx="760" cy="110" r="170" fill="rgba(255,255,255,.12)"/>
          <path d="M0 430 Q190 320 380 430 T760 400 T960 420 V540 H0Z" fill="rgba(0,0,0,.28)"/>
          <text x="52" y="430" fill="white" font-family="Arial,sans-serif" font-size="42" font-weight="700">${title}</text>
        </svg>`;
        return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(markup)}`;
    };

    const imageMap = new Map();
    const imageFor = (ref) => {
        const key = ref?.key || 'preview-default';
        if (!imageMap.has(key)) {
            const index = imageMap.size % 5;
            const palettes = [
                ['#183a52', '#58a6a6'],
                ['#56344d', '#c47a5a'],
                ['#182d46', '#6f84c4'],
                ['#4c3c26', '#b69b5d'],
                ['#202c2b', '#4f9f83'],
            ];
            imageMap.set(key, svg(ref?.title || 'MediaStationGo', ...palettes[index]));
        }
        return imageMap.get(key);
    };

    const ref = (key, title) => ({ key, type: 'backdrop', title });
    const episode = (index, title, resumePositionMs = 0) => ({
        id: `episode-${index}`,
        type: 'Episode',
        title,
        seriesName: '夜航档案',
        seriesId: 'series-night-archive',
        parentIndexNumber: 1,
        indexNumber: index,
        year: 2025,
        durationMs: 2680000,
        resumePositionMs,
        playable: true,
        landscapeImage: ref(`episode-${index}`, `第 ${index} 集`),
        primaryImage: ref(`episode-${index}`, `第 ${index} 集`),
    });
    const episodeTitles = ['潮汐线', '无人电台', '灰色航标', '最后一班船', '风暴之前', '远方来信', '沉默坐标', '夜航终点'];
    const episodes = Array.from({ length: 181 }, (_, offset) => {
        const index = offset + 1;
        return episode(index, episodeTitles[offset % episodeTitles.length], index === 121 ? 224000 : 0);
    });
    const series = {
        id: 'series-night-archive',
        type: 'Series',
        title: '夜航档案',
        year: 2025,
        seasonCount: 1,
        episodeCount: episodes.length,
        playable: false,
        overview: '一支夜间纪录片团队沿着海岸线寻找失联电台，旧录音、潮汐和一封没有寄出的信，把他们带向同一个坐标。',
        genres: ['悬疑', '剧情', '纪录片'],
        dynamicRange: 'HDR10',
        communityRating: 8.7,
        backdropImage: ref('series-night-archive-backdrop', '夜航档案'),
        landscapeImage: ref('series-night-archive-landscape', '夜航档案'),
        primaryImage: ref('series-night-archive-primary', '夜航档案'),
    };
    const movie = {
        id: 'movie-last-light', type: 'Movie', title: '最后的微光', year: 2024,
        durationMs: 7260000, playable: true, resumePositionMs: 0,
        overview: '在城市停电后的第一个清晨，一名维修工程师穿过空荡的街区。',
        genres: ['剧情'], communityRating: 8.2,
        backdropImage: ref('movie-last-light-backdrop', '最后的微光'),
        landscapeImage: ref('movie-last-light-landscape', '最后的微光'),
        primaryImage: ref('movie-last-light-primary', '最后的微光'),
    };
    const library = {
        id: 'library-films', type: 'CollectionFolder', title: '电影', playable: false,
        landscapeImage: ref('library-films', '电影'), primaryImage: ref('library-films', '电影'),
    };
    const home = {
        libraries: [library],
        resume: [episodes[120]],
        latest: [series, movie, episodes[0]],
        latestSections: [{ library, items: [series, movie, episodes[0], episodes[2]] }],
        cache: { status: 'miss' },
    };
    const details = {
        item: series,
        seasons: [{ id: 'season-1', indexNumber: 1, episodeCount: episodes.length }],
        seasonCount: 1,
        episodeCount: episodes.length,
        episodes,
        episodesBySeason: { 'season-1': episodes },
        people: [],
        selectedSeasonId: 'season-1',
    };

    const account = {
        configured: true,
        accountId: 'preview-account',
        serverId: 'preview-server',
        userId: 'preview-user',
        userName: '预览用户',
        baseUrl: 'https://preview.local',
        clientProfile: 'mediastation_windows',
    };
    window.jmpInfo = {
        version: '1.0.1-preview',
        settings: { advanced: { autoUpdateCheck: false, mediaStationProxyMode: 'direct', hideScrollbar: false } },
    };

    const respond = (requestId, operation, payload, ok = true, delay = 70) => {
        window.setTimeout(() => window._onMediaStationResponse(requestId, operation, ok, JSON.stringify(payload)), delay);
    };
    const requestOperation = (requestId, operation, raw) => {
        let input = {};
        try { input = JSON.parse(raw || '{}'); } catch { input = {}; }
        if (operation === 'home') return respond(requestId, operation, home);
        if (operation === 'detail') return respond(requestId, operation, details);
        if (operation === 'series_episodes') return respond(requestId, operation, { episodes });
        if (operation === 'cache_stats') return respond(requestId, operation, { imageBytes: imageMap.size * 42000, imageCount: imageMap.size });
        if (operation === 'clear_image_cache') return respond(requestId, operation, { imageBytes: 0, imageCount: 0 });
        if (operation === 'filters') return respond(requestId, operation, { itemTypes: ['Movie', 'Series'], genres: ['剧情', '悬疑'] });
        if (operation === 'items') return respond(requestId, operation, { startIndex: 0, nextStartIndex: 3, totalRecordCount: 3, items: [movie, series, episodes[0]] });
        if (operation === 'search') return respond(requestId, operation, { items: [series, movie], totalRecordCount: 2 });
        if (operation === 'person_items') return respond(requestId, operation, { startIndex: 0, nextStartIndex: 0, totalRecordCount: 0, items: [] });
        return respond(requestId, operation, {});
    };

    window.jmpNative = {
        mediaStationSessionStatus(requestId) { respond(requestId, 'session_status', account); },
        mediaStationCatalog(requestId, operation, raw) { requestOperation(requestId, operation, raw); },
        mediaStationImage(requestId, raw) {
            let input = {};
            try { input = JSON.parse(raw || '{}'); } catch { input = {}; }
            respond(requestId, 'image', { key: input.key, dataUrl: imageFor(input) });
        },
        mediaStationFrameInterpolation(requestId, operation) {
            if (operation === 'frame_interpolation_status') {
                return respond(requestId, operation, { componentStatus: 'ready', selectedModel: 'rife-v4.26-scale0.5', gpuName: 'Preview GPU', runtimeVersion: 'mock', engineCount: 2, models: ['rife-v4.26', 'rife-v4.26-scale0.5'] });
            }
            return respond(requestId, operation, { active: null, status: { componentStatus: 'ready', selectedModel: 'rife-v4.26-scale0.5' }, playback: {} });
        },
        mediaStationSetProxyMode(requestId) { respond(requestId, 'set_proxy_mode', account); },
        mediaStationLoad(requestId, mediaId) {
            const card = [...episodes, movie].find((item) => item.id === mediaId) || movie;
            respond(requestId, 'load', {
                durationMs: card.durationMs, deliveryMode: 'direct_cdn', probeStatus: 'skipped', acceptsRanges: true,
                redirectCount: 1, resolveMs: 42, reused: true, targetHost: 'preview.local', container: 'mkv', bitrate: 8000000,
                sourceVideo: { codec: 'H.264', profile: 'High', width: 1920, height: 1080, frameRate: 23.976, dynamicRange: 'SDR', colorSpace: 'bt709', colorTransfer: 'bt709', colorRange: 'limited', bitDepth: 8 },
                audioTracks: [{ key: 'audio-1', language: '中文', codec: 'AAC', channels: 2 }],
                subtitleTracks: [{ key: 'subtitle-1', language: '简体中文', codec: 'SRT', title: '简体中文' }],
                audioTrackKey: 'audio-1', subtitleTrackKey: 'subtitle-1', subtitleEnabled: true,
                frameInterpolation: null, mediaMetadataPending: false,
            });
            window.setTimeout(() => window._onMediaStationResponse('', 'playback_event', true, JSON.stringify({ event: 'started' })), 260);
        },
        mediaStationTracks(requestId) { respond(requestId, 'tracks', { audioTracks: [{ key: 'audio-1', language: '中文', codec: 'AAC', channels: 2 }], subtitleTracks: [{ key: 'subtitle-1', language: '简体中文', codec: 'SRT', title: '简体中文' }], audioTrackKey: 'audio-1', subtitleTrackKey: 'subtitle-1', subtitleEnabled: true, mediaMetadataPending: false }); },
        mediaStationSelectTrack(requestId, operation) { respond(requestId, operation, { ok: true }); },
        mediaStationAuthenticate(requestId) { respond(requestId, 'authenticate', account); },
        mediaStationUpdateAccount(requestId) { respond(requestId, 'update_account', account); },
        mediaStationListAccounts(requestId) { respond(requestId, 'list_accounts', { accounts: [account] }); },
        mediaStationSwitchAccount(requestId) { respond(requestId, 'switch_account', account); },
        mediaStationDeleteAccount(requestId) { respond(requestId, 'delete_account', {}); },
        setSettingValue() {},
        playerPlay() {}, playerPause() {}, playerStop() {}, playerSeek() {}, playerSetVolume() {}, playerSetMuted() {}, playerSetSpeed() {}, playerSetSubtitleStyle() {}, toggleFullscreen() {}, appExit() {},
        updateCheck() {
            window._onAppUpdateStatus('checking', '{}');
            window.setTimeout(() => window._onAppUpdateStatus('available', JSON.stringify({ version: '1.0.2', assetName: 'MediaStationGo-1.0.2-windows-x64-setup.exe', assetBytes: 214748364, packageKind: 'installer' })), 650);
        },
        updateDownload() {
            window._onAppUpdateStatus('downloading', JSON.stringify({ version: '1.0.2', percent: 18, downloadedBytes: 38654705, totalBytes: 214748364 }));
            window.setTimeout(() => window._onAppUpdateStatus('ready', JSON.stringify({ version: '1.0.2', packageKind: 'installer' })), 950);
        },
        updateInstall() { window._onAppUpdateStatus('installing', JSON.stringify({ version: '1.0.2', packageKind: 'installer' })); },
    };
})();
'@

function Send-Response($context, [int]$status, [string]$contentType, [byte[]]$bytes) {
    $context.Response.StatusCode = $status
    $context.Response.ContentType = $contentType
    $context.Response.ContentLength64 = $bytes.Length
    $context.Response.AddHeader('Cache-Control', 'no-store')
    $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
    $context.Response.Close()
}

$listener = [System.Net.HttpListener]::new()
$listener.Prefixes.Add("http://127.0.0.1:$Port/")
$listener.Start()
Write-Host "MediaStationGo 纯前端预览: http://127.0.0.1:$Port/"
Write-Host '按 Ctrl+C 停止预览。'

try {
    while ($listener.IsListening) {
        $context = $listener.GetContext()
        try {
            $path = [Uri]::UnescapeDataString($context.Request.Url.AbsolutePath)
            if ($path -in @('/', '/index.html')) {
                $html = Get-Content -LiteralPath (Join-Path $WebRoot 'mediastation.html') -Raw
                $html = $html.Replace('<script src="mediastation.js"></script>', '<script src="/preview-shim.js"></script><script src="/mediastation.js"></script>')
                Send-Response $context 200 'text/html; charset=utf-8' ([Text.Encoding]::UTF8.GetBytes($html))
                continue
            }
            if ($path -eq '/preview-shim.js') {
                Send-Response $context 200 'application/javascript; charset=utf-8' ([Text.Encoding]::UTF8.GetBytes($PreviewShim))
                continue
            }
            $relative = $path.TrimStart('/') -replace '/', '\\'
            $file = Join-Path $WebRoot $relative
            if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
                Send-Response $context 404 'text/plain; charset=utf-8' ([Text.Encoding]::UTF8.GetBytes('Not found'))
                continue
            }
            $bytes = [IO.File]::ReadAllBytes($file)
            $contentType = switch ([IO.Path]::GetExtension($file).ToLowerInvariant()) {
                '.css' { 'text/css; charset=utf-8' }
                '.js' { 'application/javascript; charset=utf-8' }
                '.png' { 'image/png' }
                default { 'application/octet-stream' }
            }
            Send-Response $context 200 $contentType $bytes
        } catch {
            try { Send-Response $context 500 'text/plain; charset=utf-8' ([Text.Encoding]::UTF8.GetBytes($_.Exception.Message)) } catch {}
        }
    }
} finally {
    $listener.Stop()
    $listener.Close()
}
