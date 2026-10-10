param(
    [int]$Port = 4173,
    [string]$CatalogPath = ''
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Get-Item $PSScriptRoot).Parent.Parent.FullName
$WebRoot = Join-Path $RepoRoot 'src\web'

$PreviewCatalogJson = 'null'
$PreviewArtworkRoot = $null
if ($CatalogPath) {
    $catalogFile = Get-Item -LiteralPath $CatalogPath -ErrorAction Stop
    $PreviewCatalogJson = Get-Content -LiteralPath $catalogFile.FullName -Raw -Encoding UTF8
    $catalog = $PreviewCatalogJson | ConvertFrom-Json
    if ($catalog.schemaVersion -ne 1 -or -not $catalog.media -or -not $catalog.libraries) {
        throw '客户端缓存预览数据格式无效'
    }
    $PreviewArtworkRoot = Join-Path $catalogFile.Directory.FullName 'images'
    $PreviewCatalogJson = $PreviewCatalogJson.Replace('<', '\u003c')
}

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

    const ref = (key, title, type = 'backdrop') => ({ key, type, title });
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
        landscapeImage: ref('series-night-archive-landscape', '剧集公共背景'),
        primaryImage: ref(`episode-${index}`, `第 ${index} 集`, 'primary'),
    });
    const episodeTitles = ['潮汐线', '无人电台', '灰色航标', '最后一班船', '风暴之前', '远方来信', '沉默坐标', '夜航终点'];
    const episodes = Array.from({ length: 181 }, (_, offset) => {
        const index = offset + 1;
        const resumePositionMs = index === 1
            ? 97042
            : index === 121
                ? 224000
                : index === 151
                    ? 250383
                    : 0;
        return episode(index, episodeTitles[offset % episodeTitles.length], resumePositionMs);
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
        resume: [
            episodes[150],
            ...episodes.slice(0, 20).map((item) => ({
                ...item,
                resumePositionMs: item.resumePositionMs || 120000,
            })),
        ],
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

    // Explicit preview-only fixture adapter. No account, token or stream URL is imported.
    const catalog = window.__frontendPreviewCatalog;
    const artwork = (key, type) => key ? { key, type } : null;
    const adapt = (item) => ({
        id: item.id, title: item.title, type: item.type, year: item.year,
        playable: ['Movie', 'Episode', 'Video'].includes(item.type),
        overview: item.description || '', durationMs: item.durationMs || 0,
        resumePositionMs: item.positionMs || 0, communityRating: item.rating || 0,
        genres: item.genres || [], seriesId: item.seriesId, seriesName: item.seriesName,
        parentIndexNumber: item.seasonNumber, indexNumber: item.episodeNumber,
        primaryImage: artwork(item.poster, 'primary'),
        landscapeImage: artwork(item.art, 'backdrop'), backdropImage: artwork(item.art, 'backdrop'),
    });
    const previewCards = catalog ? catalog.media.map(adapt) : [movie, series, ...episodes];
    const previewLibraries = catalog ? catalog.libraries.map((item) => ({
        id: item.id, title: item.title, type: 'CollectionFolder', collectionType: item.collectionType,
        primaryImage: artwork(item.cover, 'primary'), landscapeImage: artwork(item.cover, 'primary'),
    })) : [library];
    const previewHome = catalog ? {
        libraries: previewLibraries,
        resume: catalog.resumeIds.map((id) => previewCards.find((item) => item.id === id)).filter(Boolean),
        latest: catalog.latestIds.map((id) => previewCards.find((item) => item.id === id)).filter(Boolean),
        latestSections: previewLibraries.map((library, index) => ({ library,
            items: catalog.libraries[index].itemIds.map((id) => previewCards.find((item) => item.id === id)).filter(Boolean),
        })), cache: { status: 'hit' },
    } : home;
    const previewDetail = (id) => {
        if (!catalog) return id === movie.id ? { ...details, item: movie, seasons: [], episodes: [], episodesBySeason: {} } : details;
        const item = previewCards.find((card) => card.id === id);
        if (!item) throw new Error('缓存中没有该作品');
        const cached = catalog.details[id];
        const bySeason = Object.fromEntries(Object.entries(cached?.episodesBySeason || {}).map(([key, items]) => [key, items.map(adapt)]));
        return { item, seasons: cached?.seasons || [], seasonCount: cached?.seasons?.length || 0,
            episodesBySeason: bySeason, episodes: Object.values(bySeason).flat(), people: [],
            selectedSeasonId: cached?.seasons?.[0]?.id || '',
        };
    };
    const playbackEvent = (kind, fields = {}) => respond('', 'playback_event', { kind, ...fields }, true, 0);
    const respond = (requestId, operation, payload, ok = true, delay = 70) => {
        window.setTimeout(() => window._onMediaStationResponse(requestId, operation, ok, JSON.stringify(payload)), delay);
    };
    const requestOperation = (requestId, operation, raw) => {
        let input = {};
        try { input = JSON.parse(raw || '{}'); } catch { input = {}; }
        if (operation === 'favorites' || operation === 'set_favorite') return respond(requestId, operation, { code: 'preview_unavailable', message: '缓存没有收藏状态；此预览不连接服务器，不模拟收藏结果。' }, false);
        if (operation === 'home') return respond(requestId, operation, previewHome);
        if (operation === 'detail') return respond(requestId, operation, previewDetail(input.mediaId));
        if (operation === 'series_episodes') return respond(requestId, operation, { episodes: catalog ? previewDetail(input.seriesId).episodes : episodes });
        if (operation === 'cache_stats') return respond(requestId, operation, { imageBytes: imageMap.size * 42000, imageCount: imageMap.size });
        if (operation === 'clear_image_cache') return respond(requestId, operation, { imageBytes: 0, imageCount: 0 });
        const currentLibrary = catalog?.libraries.find((library) => library.id === input.parentId);
        const libraryCards = currentLibrary ? currentLibrary.itemIds.map((id) => previewCards.find((item) => item.id === id)).filter(Boolean) : [movie, series];
        if (operation === 'filters') return respond(requestId, operation, {
            itemTypes: [...new Set(libraryCards.map((item) => item.type))], genres: [...new Set(libraryCards.flatMap((item) => item.genres))].sort(),
        });
        if (operation === 'items') {
            let items = libraryCards.filter((item) => !input.genre || item.genres.includes(input.genre));
            if (input.sort === 'SortName') items.sort((a, b) => a.title.localeCompare(b.title, 'zh-CN'));
            if (input.sort === 'ProductionYear') items.sort((a, b) => (b.year || 0) - (a.year || 0));
            if (input.sort === 'CommunityRating') items.sort((a, b) => b.communityRating - a.communityRating);
            const startIndex = input.startIndex || 0; const page = items.slice(startIndex, startIndex + (input.limit || 48));
            return respond(requestId, operation, { startIndex, nextStartIndex: startIndex + page.length, totalRecordCount: items.length, items: page });
        }
        if (operation === 'search') return respond(requestId, operation, { items: previewCards.filter((card) => card.title.includes(input.query || '')), totalRecordCount: previewCards.filter((card) => card.title.includes(input.query || '')).length });
        if (operation === 'person_items') return respond(requestId, operation, { startIndex: 0, nextStartIndex: 0, totalRecordCount: 0, items: [] });
        return respond(requestId, operation, {});
    };

    window.jmpNative = {
        mediaStationSessionStatus(requestId) { respond(requestId, 'session_status', account); },
        mediaStationCatalog(requestId, operation, raw) { requestOperation(requestId, operation, raw); },
        async mediaStationImage(requestId, raw) {
            const input = JSON.parse(raw || '{}');
            try {
                let dataUrl;
                if (catalog) {
                    const filename = input.key.split('/').pop();
                    const response = await fetch('/preview-art/' + encodeURIComponent(filename));
                    if (!response.ok) throw new Error('缓存图片不存在');
                    const bytes = new Uint8Array(await response.arrayBuffer());
                    let binary = '';
                    for (let i = 0; i < bytes.length; i += 16384) binary += String.fromCharCode(...bytes.subarray(i, i + 16384));
                    dataUrl = 'data:' + response.headers.get('Content-Type') + ';base64,' + btoa(binary);
                } else dataUrl = imageFor(input);
                respond(requestId, 'image', { key: input.key, dataUrl });
            } catch (error) { respond(requestId, 'image', { code: 'preview_image_missing', message: error.message }, false); }
        },
        mediaStationFrameInterpolation(requestId, operation) {
            if (operation === 'frame_interpolation_status') {
                return respond(requestId, operation, { component: { installed: true, supported: true }, componentStatus: 'ready', selectedModel: 'rife-v4.26-scale0.5', gpuName: 'Preview GPU', runtimeVersion: 'mock', engineCount: 2, models: ['rife-v4.26', 'rife-v4.26-scale0.5'] });
            }
            return respond(requestId, operation, { active: null, status: { component: { installed: true, supported: true }, componentStatus: 'ready', selectedModel: 'rife-v4.26-scale0.5' }, playback: {} });
        },
        mediaStationSetProxyMode(requestId) { respond(requestId, 'set_proxy_mode', account); },
        mediaStationLoad(requestId, mediaId) {
            if (new URLSearchParams(location.search).get('playback') === 'error') return respond(requestId, 'load', { code: 'preview_error', message: '播放地址获取失败（开发预览模拟）' }, false, 1400);
            const card = previewCards.find((item) => item.id === mediaId) || Object.values(catalog?.details || {}).flatMap((detail) => Object.values(detail.episodesBySeason || {}).flat()).map(adapt).find((item) => item.id === mediaId);
            if (!card) return respond(requestId, 'load', { message: '预览缓存中没有该作品' }, false);
            respond(requestId, 'load', {
                durationMs: card.durationMs, deliveryMode: 'direct_cdn', probeStatus: 'skipped', acceptsRanges: true,
                redirectCount: 1, resolveMs: 42, reused: true, targetHost: 'preview.local', container: 'mkv', bitrate: 8000000,
                sourceVideo: { codec: 'H.264', profile: 'High', width: 1920, height: 1080, frameRate: 23.976, dynamicRange: 'SDR', colorSpace: 'bt709', colorTransfer: 'bt709', colorRange: 'limited', bitDepth: 8 },
                audioTracks: [{ key: 'audio-1', language: '中文', codec: 'AAC', channels: 2 }],
                subtitleTracks: [{ key: 'subtitle-1', language: '简体中文', codec: 'SRT', title: '简体中文' }],
                audioTrackKey: 'audio-1', subtitleTrackKey: 'subtitle-1', subtitleEnabled: true,
                frameInterpolation: null, mediaMetadataPending: false,
            });
            window.setTimeout(() => window._onMediaStationResponse('', 'playback_event', true, JSON.stringify({ kind: 'started' })), 1800);
        },
        mediaStationTracks(requestId) { respond(requestId, 'tracks', { audioTracks: [{ key: 'audio-1', language: '中文', codec: 'AAC', channels: 2 }], subtitleTracks: [{ key: 'subtitle-1', language: '简体中文', codec: 'SRT', title: '简体中文' }], audioTrackKey: 'audio-1', subtitleTrackKey: 'subtitle-1', subtitleEnabled: true, mediaMetadataPending: false }); },
        mediaStationSelectTrack(requestId, operation) { respond(requestId, operation, { ok: true }); },
        mediaStationAuthenticate(requestId) { respond(requestId, 'authenticate', account); },
        mediaStationUpdateAccount(requestId) { respond(requestId, 'update_account', account); },
        mediaStationListAccounts(requestId) { respond(requestId, 'list_accounts', { accounts: [account] }); },
        mediaStationSwitchAccount(requestId) { respond(requestId, 'switch_account', account); },
        mediaStationDeleteAccount(requestId) { respond(requestId, 'delete_account', {}); },
        setSettingValue() {},
        playerPlay() { playbackEvent('started'); }, playerPause() { playbackEvent('paused'); }, playerStop() { playbackEvent('finished'); }, playerSeek(positionMs) { playbackEvent('position', { positionMs }); }, playerSetVolume() {}, playerSetMuted() {}, playerSetSpeed() {}, playerSetSubtitleStyle() {}, toggleFullscreen() {}, appExit() {},
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
                $html = $html.Replace('</head>', '<style>html.player-mode,html.player-mode body{background:#080b10!important}</style></head>')
                $html = $html.Replace('<body>', '<body><div style="position:fixed;z-index:190;right:12px;top:4px;font:10px Segoe UI;color:#64748b;pointer-events:none">缓存预览 · 未连接服务器 · 播放状态模拟</div>')
                $fixtureScript = '<script>window.__frontendPreviewCatalog=' + $PreviewCatalogJson + ';</script>'
                $html = $html.Replace('<script src="mediastation.js"></script>', $fixtureScript + '<script src="/preview-shim.js"></script><script src="/mediastation.js"></script>')
                Send-Response $context 200 'text/html; charset=utf-8' ([Text.Encoding]::UTF8.GetBytes($html))
                continue
            }
            if ($path -eq '/preview-shim.js') {
                Send-Response $context 200 'application/javascript; charset=utf-8' ([Text.Encoding]::UTF8.GetBytes($PreviewShim))
                continue
            }
            if ($path.StartsWith('/preview-art/') -and $PreviewArtworkRoot) {
                $filename = $path.Substring('/preview-art/'.Length)
                if ($filename -notmatch '^[a-f0-9]{64}\.(jpg|png)$') {
                    Send-Response $context 400 'text/plain' ([Text.Encoding]::UTF8.GetBytes('Invalid artwork name'))
                    continue
                }
                $artFile = Join-Path $PreviewArtworkRoot $filename
                if (-not (Test-Path -LiteralPath $artFile -PathType Leaf)) {
                    Send-Response $context 404 'text/plain' ([Text.Encoding]::UTF8.GetBytes('Artwork not cached'))
                    continue
                }
                $mime = if ($filename.EndsWith('.png')) { 'image/png' } else { 'image/jpeg' }
                Send-Response $context 200 $mime ([IO.File]::ReadAllBytes($artFile))
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
                '.jpg' { 'image/jpeg' }
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
