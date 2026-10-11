(() => {
    'use strict';

    const byId = (id) => document.getElementById(id);
    const splash = byId('splash');
    const loginView = byId('login-view');
    const appShell = byId('app-shell');
    const appBackdrop = byId('app-backdrop');
    const appBackdropLayers = [...appBackdrop.querySelectorAll('.app-backdrop-layer')];
    const topbar = document.querySelector('.topbar');
    const content = byId('content');
    const playerView = byId('player-view');
    const playerDanmakuLayer = byId('player-danmaku-layer');
    const playerControls = byId('player-controls');
    const playerPanel = byId('player-panel');
    const playerPanelContent = byId('player-panel-content');
    const toast = byId('toast');
    const pending = new Map();
    const imageCache = new Map();
    const imagePending = new Map();
    const imageQueue = [];
    const libraryCache = new Map();
    const libraryBrowsePreferences = new Map();
    const seriesDetailCache = new Map();
    const detailRefreshRequests = new Set();
    const scrollMotions = new WeakMap();
    const history = [];
    const homeRefreshDelayMs = 1200;
    const heroRotationIntervalMs = 9000;
    const heroCarouselMaxCards = 4;
    const heroArtworkCache = new Map();
    const maximumContinueWatchingItems = 20;
    const libraryPageSize = 48;
    const maximumLibraryCacheEntries = 12;
    const maximumSeriesDetailCacheEntries = 12;
    const detailRefreshDelayMs = 1200;
    const playerPlaybackSettingsKey = 'MediaStationGo.Windows.playbackSettingsByMedia.v2';
    const legacyPlayerPlaybackSettingsKey = 'MediaStationGo.Windows.playbackSettingsByMedia.v1';
    const defaultSubtitleStyleSettingKey = 'MediaStationGo.Windows.defaultSubtitleStyle.v2';
    const legacyDefaultSubtitleStyleSettingKey = 'MediaStationGo.Windows.defaultSubtitleStyle.v1';
    const danmakuEnabledSettingKey = 'MediaStationGo.Windows.danmakuEnabled.v1';
    const danmakuStyleSettingKey = 'MediaStationGo.Windows.danmakuStyle.v1';
    const seriesPlaybackSettingsKey = 'MediaStationGo.Windows.seriesPlaybackSettings.v1';
    const uiThemeSettingKey = 'MediaStationGo.Windows.uiTheme.v1';
    const playerSubtitleStyleDefaults = Object.freeze({ fontSize: 36, bottomOffset: 8 });
    const danmakuStyleDefaults = Object.freeze({
        fontSize: 25,
        opacity: 80,
        displayArea: 80,
        scrollSeconds: 12,
        fixedSeconds: 4,
    });
    const danmakuStyleControlDefinitions = Object.freeze([
        { property: 'fontSize', id: 'player-danmaku-font-size', outputId: 'player-danmaku-font-size-value', min: 20, max: 40, suffix: ' px' },
        { property: 'opacity', id: 'player-danmaku-opacity', outputId: 'player-danmaku-opacity-value', min: 35, max: 100, suffix: '%' },
        { property: 'displayArea', id: 'player-danmaku-display-area', outputId: 'player-danmaku-display-area-value', min: 30, max: 100, suffix: '%' },
        { property: 'scrollSeconds', id: 'player-danmaku-scroll-seconds', outputId: 'player-danmaku-scroll-seconds-value', min: 6, max: 20, suffix: ' 秒' },
        { property: 'fixedSeconds', id: 'player-danmaku-fixed-seconds', outputId: 'player-danmaku-fixed-seconds-value', min: 3, max: 8, suffix: ' 秒' },
    ]);
    const seriesPlaybackSettingDefaults = Object.freeze({
        autoNext: true,
        introSkipSeconds: 0,
        outroSkipSeconds: 0,
    });
    const playerPlaybackRates = Object.freeze([0.5, 0.75, 1, 1.25, 1.5, 2]);
    const interpolationModels = Object.freeze({
        'rife-v4.26': { label: '质量优先', name: 'RIFE v4.26' },
        'rife-v4.26-scale0.5': { label: '均衡优先', name: 'RIFE v4.26 · Scale 0.5' },
        'rife-v4.25-lite': { label: '流畅优先', name: 'RIFE v4.25 Lite' },
    });
    const languageNameMap = Object.freeze({
        ar: '阿拉伯语', ara: '阿拉伯语',
        bg: '保加利亚语', bul: '保加利亚语',
        cs: '捷克语', ces: '捷克语', cze: '捷克语',
        da: '丹麦语', dan: '丹麦语',
        de: '德语', deu: '德语', ger: '德语', german: '德语',
        el: '希腊语', ell: '希腊语', gre: '希腊语',
        en: '英语', eng: '英语', english: '英语',
        es: '西班牙语', spa: '西班牙语', spanish: '西班牙语',
        fi: '芬兰语', fin: '芬兰语',
        fr: '法语', fra: '法语', fre: '法语', french: '法语',
        he: '希伯来语', heb: '希伯来语',
        hi: '印地语', hin: '印地语',
        hu: '匈牙利语', hun: '匈牙利语',
        id: '印度尼西亚语', ind: '印度尼西亚语',
        it: '意大利语', ita: '意大利语', italian: '意大利语',
        ja: '日语', jpn: '日语', japanese: '日语',
        ko: '韩语', kor: '韩语', korean: '韩语',
        ms: '马来语', msa: '马来语', may: '马来语',
        nl: '荷兰语', nld: '荷兰语', dut: '荷兰语',
        no: '挪威语', nor: '挪威语',
        pl: '波兰语', pol: '波兰语',
        pt: '葡萄牙语', por: '葡萄牙语', portuguese: '葡萄牙语',
        ro: '罗马尼亚语', ron: '罗马尼亚语', rum: '罗马尼亚语',
        ru: '俄语', rus: '俄语', russian: '俄语',
        sv: '瑞典语', swe: '瑞典语',
        th: '泰语', tha: '泰语',
        tr: '土耳其语', tur: '土耳其语',
        uk: '乌克兰语', ukr: '乌克兰语',
        vi: '越南语', vie: '越南语',
    });
    let imageActive = 0;
    let imageBackgroundActive = 0;
    let imageDiskStats = { imageBytes: 0, imageCount: 0 };
    let imageStatsTimer = 0;
    let interpolationComponentTimer = 0;
    let interpolationComponentTracking = false;
    let interpolationRestartPrompted = false;
    let interpolationComponentRestarting = false;
    let requestSequence = 0;
    let toastTimer = 0;
    let drawerTrigger = null;
    let currentDrawer = null;
    let drawerScrimCloseTimer = 0;
    let settingsCardAnimation = null;
    let settingsCardAnimationFrame = 0;
    let settingsMotionLayer = null;
    const settingsCardOpenMotionDuration = 280;
    const settingsCardCloseMotionDuration = 220;
    let currentView = null;
    let session = null;
    let homeData = null;
    let heroRevision = 0;
    let appBackdropRevision = 0;
    let appBackdropSrc = '';
    let heroSlideTimer = 0;
    let controlsTimer = 0;
    let feedbackTimer = 0;
    let playerClickTimer = 0;
    let playerClickAt = 0;
    let playerClickX = 0;
    let playerClickY = 0;
    let seekRepeatCount = 0;
    let player = null;
    let playbackFailure = null;
    let playerPanelTrigger = null;
    let overviewTrigger = null;
    let sourcePickerTrigger = null;
    let loginCanCancel = false;
    let loginReturnFocus = null;
    let loginMode = 'add';
    let loginAccount = null;
    let loginAccountBaseUrl = '';
    let loginConnectionLocked = false;
    let savedAccountsRevision = 0;
    let pendingDeleteAccount = null;
    let deleteReturnFocus = null;
    let deleteReturnSurface = 'drawer';
    let contentTransitionTimer = 0;
    let homeRefreshTimer = 0;
    let homeRefreshGeneration = 0;
    let homeRefreshFailures = 0;
    let homeVerificationPending = false;
    let heroRotationTimer = 0;
    let heroCarouselController = null;
    let libraryFilterRevision = 0;
    let personPageRevision = 0;
    let searchRevision = 0;
    let playerPlaybackSettings = loadPlayerPlaybackSettings();
    let defaultPlayerSubtitleStyle = loadDefaultPlayerSubtitleStyle();
    let danmakuEnabled = loadDanmakuEnabled();
    let danmakuStyle = loadDanmakuStyle();
    let seriesPlaybackSettings = loadSeriesPlaybackSettings();
    let playerVolume = 100;
    let lastAudiblePlayerVolume = 100;
    let playerSubtitleStyle = { ...playerSubtitleStyleDefaults };
    let playerSubtitleStyleCustomized = false;
    let playerPlaybackRate = 1;
    let preferredInterpolationModel = 'rife-v4.26';
    let autoUpdateCheckEnabled = window.jmpInfo?.settings?.advanced?.autoUpdateCheck !== false;
    let updateDownloadSources = window.jmpInfo?.settings?.advanced?.updateDownloadSources || '';
    let updateDownloadSourceMode = window.jmpInfo?.settings?.advanced?.updateDownloadSourceMode || 'server';
    let globalProxyMode = window.jmpInfo?.settings?.advanced?.mediaStationProxyMode === 'system'
        ? 'system'
        : 'direct';
    let activeSettingsSection = 'appearance';
    let proxySettingsStatus = { state: '', text: '' };
    let appUpdateState = { status: 'idle', payload: {} };
    let automaticUpdateTimer = 0;
    let updateNotificationVersion = '';
    const danmakuRenderer = createDanmakuRenderer(playerDanmakuLayer);

    const imageObserver = new IntersectionObserver((entries) => {
        for (const entry of entries) {
            if (!entry.isIntersecting) continue;
            imageObserver.unobserve(entry.target);
            loadObservedImage(entry.target);
        }
    }, { rootMargin: '260px' });

    const libraryPageObserver = new IntersectionObserver((entries) => {
        if (!entries.some((entry) => entry.isIntersecting)) return;
        if (currentView?.kind === 'library') loadMoreLibrary(currentView.data);
    }, { root: content, rootMargin: '900px 0px' });
    const personPageObserver = new IntersectionObserver((entries) => {
        if (!entries.some((entry) => entry.isIntersecting)) return;
        if (currentView?.kind === 'person') loadMorePerson(currentView.data);
    }, { root: content, rootMargin: '900px 0px' });

    function createDanmakuRenderer(layer) {
        const canvas = document.createElement('canvas');
        canvas.className = 'player-danmaku-canvas';
        canvas.setAttribute('aria-hidden', 'true');
        layer.replaceChildren(canvas);
        const context = canvas.getContext('2d', { alpha: true, desynchronized: true });
        if (!context) throw new Error('Canvas 2D is required for danmaku rendering');

        const state = {
            comments: [],
            active: [],
            nextIndex: 0,
            positionMs: 0,
            clockAt: performance.now(),
            playing: false,
            rate: 1,
            enabled: true,
            frame: 0,
            style: { ...danmakuStyleDefaults },
            width: 0,
            height: 0,
            pixelRatio: 0,
        };
        const BILI_PLAYER_WIDTH = 682;
        const BILI_COMMON_DURATION_MS = 3_800;
        const BILI_MIN_DURATION_MS = 4_000;
        const LANE_MARGIN = 8;

        const safeNumber = (value, fallback) => Number.isFinite(Number(value)) ? Number(value) : fallback;
        const styleFor = (style) => ({
            fontSize: Math.max(20, Math.min(40, Math.round(safeNumber(style?.fontSize, state.style.fontSize)))),
            opacity: Math.max(35, Math.min(100, safeNumber(style?.opacity, state.style.opacity))),
            displayArea: Math.max(30, Math.min(100, safeNumber(style?.displayArea, state.style.displayArea))),
            scrollSeconds: Math.max(6, Math.min(20, safeNumber(style?.scrollSeconds, state.style.scrollSeconds))),
            fixedSeconds: Math.max(3, Math.min(8, safeNumber(style?.fixedSeconds, state.style.fixedSeconds))),
        });

        function currentPositionMs() {
            if (!state.playing) return state.positionMs;
            return state.positionMs + (performance.now() - state.clockAt) * state.rate;
        }

        function syncCanvasSize() {
            const width = Math.max(0, Math.round(layer.clientWidth));
            const height = Math.max(0, Math.round(layer.clientHeight));
            const pixelRatio = Math.max(1, window.devicePixelRatio || 1);
            if (width === state.width && height === state.height && pixelRatio === state.pixelRatio) return width > 0 && height > 0;
            state.width = width;
            state.height = height;
            state.pixelRatio = pixelRatio;
            canvas.width = Math.round(width * pixelRatio);
            canvas.height = Math.round(height * pixelRatio);
            canvas.style.width = `${width}px`;
            canvas.style.height = `${height}px`;
            context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
            context.textBaseline = 'top';
            return width > 0 && height > 0;
        }

        function commentIndexAt(seconds) {
            let low = 0;
            let high = state.comments.length;
            while (low < high) {
                const middle = low + Math.floor((high - low) / 2);
                if (state.comments[middle].time < seconds) low = middle + 1;
                else high = middle;
            }
            return low;
        }

        function restartAt(positionMs) {
            state.active = [];
            const visibleWindowSeconds = Math.max(22, state.style.scrollSeconds * 2, state.style.fixedSeconds);
            state.nextIndex = commentIndexAt(Math.max(0, positionMs / 1000 - visibleWindowSeconds));
        }

        function fontSize() {
            return state.style.fontSize;
        }

        function lineHeight() {
            return Math.ceil(fontSize() * 1.25 + LANE_MARGIN);
        }

        function laneCount() {
            return Math.max(1, Math.floor(state.height * state.style.displayArea / 100 / lineHeight()));
        }

        function scrollDurationMs() {
            const biliDuration = Math.max(
                BILI_MIN_DURATION_MS,
                BILI_COMMON_DURATION_MS * state.width / BILI_PLAYER_WIDTH,
            );
            return Math.round(biliDuration * state.style.scrollSeconds / danmakuStyleDefaults.scrollSeconds);
        }

        function colorFor(value) {
            const color = Math.max(0, Math.min(0xFFFFFF, Math.trunc(safeNumber(value, 0xFFFFFF))));
            return `rgb(${(color >>> 16) & 0xFF}, ${(color >>> 8) & 0xFF}, ${color & 0xFF})`;
        }

        function textWidth(text) {
            context.font = `650 ${fontSize()}px "Microsoft YaHei", sans-serif`;
            return Math.ceil(context.measureText(text).width);
        }

        function trackEntries(mode, lane) {
            return state.active.filter((entry) => entry.mode === mode && entry.lane === lane);
        }

        function chooseFixedLane(mode, positionMs, lanes) {
            for (let lane = 0; lane < lanes; lane += 1) {
                if (!trackEntries(mode, lane).some((entry) => entry.endMs > positionMs)) return lane;
            }
            return -1;
        }

        function chooseScrollLane(mode, positionMs, durationMs, width, lanes) {
            const velocity = (state.width + width) / durationMs;
            for (let lane = 0; lane < lanes; lane += 1) {
                const previous = trackEntries(mode, lane)
                    .filter((entry) => entry.endMs > positionMs)
                    .sort((left, right) => right.startMs - left.startMs)[0];
                if (!previous) return lane;
                const elapsed = Math.max(0, positionMs - previous.startMs);
                const previousVelocity = (state.width + previous.width) / previous.durationMs;
                const previousRight = state.width - previousVelocity * elapsed + previous.width;
                const previousHasEntered = previousRight <= state.width - LANE_MARGIN;
                if (previousHasEntered && velocity <= previousVelocity) return lane;
            }
            return -1;
        }

        function addComment(comment, positionMs) {
            if (!syncCanvasSize()) return;
            const mode = Number(comment.mode) || 1;
            const fixed = mode === 4 || mode === 5;
            const durationMs = fixed ? state.style.fixedSeconds * 1000 : scrollDurationMs();
            const elapsedMs = Math.max(0, positionMs - comment.time * 1000);
            if (elapsedMs >= durationMs) return;
            const lanes = laneCount();
            const width = textWidth(comment.text);
            const lane = fixed
                ? chooseFixedLane(mode, positionMs, lanes)
                : chooseScrollLane(mode, positionMs, durationMs, width, lanes);
            if (lane < 0) return;
            const height = lineHeight();
            let top;
            if (fixed) {
                const top = mode === 5
                    ? lane * height
                    : Math.max(0, state.height - (lane + 1) * height);
                state.active.push({
                    mode,
                    lane,
                    text: comment.text,
                    color: colorFor(comment.color),
                    width,
                    top,
                    startMs: comment.time * 1000,
                    endMs: comment.time * 1000 + durationMs,
                    durationMs,
                });
            } else {
                top = lane * height;
                state.active.push({
                    mode,
                    lane,
                    text: comment.text,
                    color: colorFor(comment.color),
                    width,
                    top,
                    startMs: comment.time * 1000,
                    endMs: comment.time * 1000 + durationMs,
                    durationMs,
                });
            }
        }

        function draw(positionMs) {
            if (!syncCanvasSize()) return;
            context.clearRect(0, 0, state.width, state.height);
            context.font = `650 ${fontSize()}px "Microsoft YaHei", sans-serif`;
            context.lineJoin = 'round';
            context.miterLimit = 2;
            context.lineWidth = Math.max(1, fontSize() * 0.08);
            const alpha = state.style.opacity / 100;
            for (const entry of state.active) {
                const elapsed = positionMs - entry.startMs;
                if (elapsed < 0 || elapsed >= entry.durationMs) continue;
                let left;
                if (entry.mode === 4 || entry.mode === 5) {
                    left = Math.max(0, (state.width - entry.width) / 2);
                } else {
                    const progress = elapsed / entry.durationMs;
                    const distance = state.width + entry.width;
                    left = entry.mode === 6
                        ? -entry.width + distance * progress
                        : state.width - distance * progress;
                }
                context.globalAlpha = alpha;
                context.strokeStyle = '#000';
                context.fillStyle = entry.color;
                context.strokeText(entry.text, left, entry.top);
                context.fillText(entry.text, left, entry.top);
            }
            context.globalAlpha = 1;
        }

        function render(positionMs) {
            if (!state.enabled || !state.comments.length || !layer.isConnected) return;
            while (state.nextIndex < state.comments.length
                && state.comments[state.nextIndex].time * 1000 <= positionMs) {
                addComment(state.comments[state.nextIndex], positionMs);
                state.nextIndex += 1;
            }
            state.active = state.active.filter((entry) => entry.endMs > positionMs);
            draw(positionMs);
        }

        function tick() {
            state.frame = 0;
            const positionMs = currentPositionMs();
            render(positionMs);
            if (state.playing && state.enabled && (state.nextIndex < state.comments.length || state.active.length)) schedule();
        }

        function schedule() {
            if (!state.frame) state.frame = requestAnimationFrame(tick);
        }

        function setClock(positionMs, playing, rate, restart) {
            const now = performance.now();
            const current = currentPositionMs();
            const nextPosition = Math.max(0, safeNumber(positionMs, current));
            const nextRate = Math.max(0.1, safeNumber(rate, state.rate));
            state.positionMs = nextPosition;
            state.clockAt = now;
            state.rate = nextRate;
            state.playing = playing === true;
            if (restart || Math.abs(nextPosition - current) > 1500) restartAt(nextPosition);
            render(nextPosition);
            if (state.playing) schedule();
        }

        new ResizeObserver(() => {
            const positionMs = currentPositionMs();
            syncCanvasSize();
            if (state.comments.length) restartAt(positionMs);
            render(positionMs);
            if (state.playing) schedule();
        }).observe(layer);

        return {
            load(payload, positionMs, playing, rate) {
                state.style = styleFor(payload.style);
                state.enabled = payload.enabled !== false;
                state.comments = Array.isArray(payload.comments)
                    ? payload.comments
                        .filter((comment) => Number.isFinite(Number(comment?.time)) && typeof comment?.text === 'string')
                        .map((comment) => ({ ...comment, time: Math.max(0, Number(comment.time)) }))
                        .sort((left, right) => left.time - right.time)
                    : [];
                setClock(positionMs, playing, rate, true);
                if (!state.enabled) clearActive();
            },
            clear() {
                if (state.frame) cancelAnimationFrame(state.frame);
                state.frame = 0;
                state.comments = [];
                state.nextIndex = 0;
                state.active = [];
                if (syncCanvasSize()) context.clearRect(0, 0, state.width, state.height);
            },
            setEnabled(enabled) {
                state.enabled = enabled === true;
                if (!state.enabled) {
                    state.active = [];
                    if (syncCanvasSize()) context.clearRect(0, 0, state.width, state.height);
                } else {
                    const positionMs = currentPositionMs();
                    restartAt(positionMs);
                    render(positionMs);
                }
                if (state.enabled && state.playing) schedule();
            },
            setStyle(style) {
                state.style = styleFor(style);
                if (state.comments.length) {
                    const positionMs = currentPositionMs();
                    restartAt(positionMs);
                    render(positionMs);
                }
                if (state.enabled && state.playing) schedule();
            },
            updatePlayback(event, playing) {
                const restart = event.kind === 'seeked';
                setClock(event.positionMs, playing, event.rate, restart);
            },
            setRate(rate) {
                setClock(currentPositionMs(), state.playing, rate, false);
            },
        };
    }

    function handleDanmakuEvent(event) {
        if (!event || typeof event !== 'object') return;
        if (event.kind === 'clear') {
            danmakuRenderer.clear();
        } else if (event.kind === 'enabled') {
            danmakuRenderer.setEnabled(event.enabled === true);
        } else if (event.kind === 'style') {
            danmakuRenderer.setStyle(event.style);
        } else if (event.kind === 'load' && player?.card?.id === event.mediaId) {
            danmakuRenderer.load(event, player.positionMs, player.playing, playerPlaybackRate);
        }
    }

    function nextRequestId(prefix) {
        requestSequence += 1;
        return `${prefix}-${Date.now().toString(36)}-${requestSequence.toString(36)}`;
    }

    function nativeRequest(nativeName, operation, args = [], timeoutMs = 30000) {
        return new Promise((resolve, reject) => {
            const bridge = window.jmpNative;
            if (!bridge || typeof bridge[nativeName] !== 'function') {
                reject(new Error('原生接口不可用'));
                return;
            }
            const requestId = nextRequestId(operation);
            const timer = window.setTimeout(() => {
                pending.delete(requestId);
                reject(new Error(`${operation} 请求超时`));
            }, timeoutMs);
            pending.set(requestId, { operation, resolve, reject, timer });
            try {
                bridge[nativeName](requestId, ...args);
            } catch (error) {
                window.clearTimeout(timer);
                pending.delete(requestId);
                reject(error instanceof Error ? error : new Error(String(error)));
            }
        });
    }

    window._onMediaStationResponse = (requestId, operation, ok, payloadJson) => {
        let payload;
        try {
            payload = JSON.parse(payloadJson || '{}');
        } catch {
            payload = { code: 'invalid_native_response', message: '原生响应格式无效' };
            ok = false;
        }
        if (operation === 'playback_event') {
            handlePlaybackEvent(payload);
            return;
        }
        if (operation === 'danmaku_event') {
            handleDanmakuEvent(payload);
            return;
        }
        const task = pending.get(requestId);
        if (!task) {
            console.error(`MediaStation response has no pending request: ${operation}`);
            return;
        }
        pending.delete(requestId);
        window.clearTimeout(task.timer);
        if (task.operation !== operation) {
            task.reject(new Error(`原生响应操作不匹配：${operation}`));
        } else if (ok) {
            task.resolve(payload);
        } else {
            const error = new Error(payload.message || '请求失败');
            error.code = payload.code || 'native_request_failed';
            task.reject(error);
        }
    };

    const previousFullscreenChanged = window._nativeFullscreenChanged;
    window._nativeFullscreenChanged = (fullscreen) => {
        if (typeof previousFullscreenChanged === 'function') previousFullscreenChanged(fullscreen);
        window._isFullscreen = fullscreen === true;
        refreshPlayerTools();
        refreshPlayerCursor();
        requestAnimationFrame(positionPlayerPanel);
    };

    function element(tag, className, text) {
        const node = document.createElement(tag);
        if (className) node.className = className;
        if (text !== undefined) node.textContent = text;
        return node;
    }

    function focusElement(node, preventScroll = true) {
        if (!node) return;
        try {
            node.focus({ preventScroll });
        } catch {
            node.focus();
        }
    }

    function clamp(value, minimum, maximum) {
        return Math.max(minimum, Math.min(maximum, value));
    }

    function scrollLimit(node, axis) {
        return axis === 'x'
            ? Math.max(0, node.scrollWidth - node.clientWidth)
            : Math.max(0, node.scrollHeight - node.clientHeight);
    }

    function cubicBezierEasing(x1, y1, x2, y2) {
        const sample = (time, first, second) => {
            const inverse = 1 - time;
            return 3 * inverse * inverse * time * first
                + 3 * inverse * time * time * second
                + time * time * time;
        };
        return (progress) => {
            let lower = 0;
            let upper = 1;
            let time = progress;
            for (let iteration = 0; iteration < 10; iteration += 1) {
                time = (lower + upper) / 2;
                if (sample(time, x1, x2) < progress) lower = time;
                else upper = time;
            }
            return sample(time, y1, y2);
        };
    }

    const scrollEasing = cubicBezierEasing(0.2, 0, 0, 1);
    const horizontalScrollDurationMs = 320;

    function stopSmoothScroll(node) {
        const motion = scrollMotions.get(node);
        if (!motion) return false;
        if (motion.frame) window.cancelAnimationFrame(motion.frame);
        scrollMotions.delete(node);
        motion.resolve(false);
        return true;
    }

    function smoothScrollTo(node, requested = {}, durationMs = horizontalScrollDurationMs) {
        if (!node) return Promise.resolve(false);
        const targetX = clamp(requested.left ?? node.scrollLeft, 0, scrollLimit(node, 'x'));
        const targetY = clamp(requested.top ?? node.scrollTop, 0, scrollLimit(node, 'y'));
        const animateX = requested.left !== undefined && Math.abs(targetX - node.scrollLeft) > 0.01;
        const animateY = requested.top !== undefined && Math.abs(targetY - node.scrollTop) > 0.01;
        stopSmoothScroll(node);
        if (prefersReducedMotion()) {
            if (requested.left !== undefined) node.scrollLeft = targetX;
            if (requested.top !== undefined) node.scrollTop = targetY;
            return Promise.resolve(true);
        }
        if (!animateX && !animateY) return Promise.resolve(true);

        const startX = node.scrollLeft;
        const startY = node.scrollTop;
        return new Promise((resolve) => {
            const motion = {
                targetX,
                targetY,
                frame: 0,
                startTime: 0,
                resolve,
            };
            scrollMotions.set(node, motion);
            const advance = (time) => {
                if (scrollMotions.get(node) !== motion) return;
                if (!node.isConnected) {
                    scrollMotions.delete(node);
                    resolve(false);
                    return;
                }
                if (!motion.startTime) motion.startTime = time;
                const progress = clamp((time - motion.startTime) / durationMs, 0, 1);
                const eased = scrollEasing(progress);
                if (animateX) node.scrollLeft = startX + (targetX - startX) * eased;
                if (animateY) node.scrollTop = startY + (targetY - startY) * eased;
                if (progress < 1) {
                    motion.frame = window.requestAnimationFrame(advance);
                    return;
                }
                if (animateX) node.scrollLeft = targetX;
                if (animateY) node.scrollTop = targetY;
                scrollMotions.delete(node);
                resolve(true);
            };
            motion.frame = window.requestAnimationFrame(advance);
        });
    }

    function revealTarget(container, node, axis, alignment) {
        const containerRect = container.getBoundingClientRect();
        const nodeRect = node.getBoundingClientRect();
        const current = axis === 'x' ? container.scrollLeft : container.scrollTop;
        const viewportSize = axis === 'x' ? container.clientWidth : container.clientHeight;
        const start = current + (axis === 'x' ? nodeRect.left - containerRect.left : nodeRect.top - containerRect.top);
        const size = axis === 'x' ? nodeRect.width : nodeRect.height;
        if (alignment === 'center') return start - (viewportSize - size) / 2;
        if (alignment === 'start') return start;
        if (start < current) return start;
        if (start + size > current + viewportSize) return start + size - viewportSize;
        return current;
    }

    function focusAndReveal(node, inline = 'center', block = 'nearest') {
        if (!node) return;
        const row = node.closest('.media-row');
        const filterOptions = node.closest('.library-filter-options');
        const segmented = node.closest('.segmented-control');
        if (row?._revealCarouselNode) row._revealCarouselNode(node);
        else if (row) smoothScrollTo(row, { left: revealTarget(row, node, 'x', inline) });
        else if (filterOptions) smoothScrollTo(filterOptions, { left: revealTarget(filterOptions, node, 'x', inline) });
        else if (segmented) smoothScrollTo(segmented, { left: revealTarget(segmented, node, 'x', inline) });
        const verticalAlignment = node.closest('.people-row .person-card') ? 'center' : block;
        smoothScrollTo(content, { top: revealTarget(content, node, 'y', verticalAlignment) });
        focusElement(node);
    }

    function showToast(message) {
        window.clearTimeout(toastTimer);
        toast.textContent = message;
        toast.classList.remove('hidden');
        toastTimer = window.setTimeout(() => toast.classList.add('hidden'), 3200);
    }

    function loadPlayerPlaybackSettings() {
        try {
            const value = window.localStorage.getItem(playerPlaybackSettingsKey);
            if (value !== null) {
                const parsed = JSON.parse(value);
                if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) return parsed;
                console.error('MediaStation playback settings are invalid');
                return {};
            }
            const legacyValue = window.localStorage.getItem(legacyPlayerPlaybackSettingsKey);
            if (legacyValue === null) return {};
            const legacySettings = JSON.parse(legacyValue);
            if (!legacySettings || typeof legacySettings !== 'object' || Array.isArray(legacySettings)) {
                console.error('MediaStation legacy playback settings are invalid');
                return {};
            }
            const migrated = {};
            for (const [scopeId, settings] of Object.entries(legacySettings)) {
                migrated[scopeId] = settings && typeof settings === 'object' && !Array.isArray(settings)
                    && isValidLegacyPlayerSubtitleStyle(settings.subtitleStyle)
                    ? {
                        ...settings,
                        subtitleStyle: {
                            fontSize: settings.subtitleStyle.fontSize,
                            bottomOffset: playerSubtitleStyleDefaults.bottomOffset,
                        },
                    }
                    : settings;
            }
            try {
                window.localStorage.setItem(playerPlaybackSettingsKey, JSON.stringify(migrated));
            } catch (error) {
                console.error(`MediaStation migrated playback settings could not be saved: ${error}`);
            }
            return migrated;
        } catch (error) {
            console.error(`MediaStation playback settings could not be read: ${error}`);
        }
        return {};
    }

    function isValidPlayerSubtitleStyle(style) {
        return Number.isInteger(style?.fontSize) && style.fontSize >= 24 && style.fontSize <= 64
            && Number.isInteger(style?.bottomOffset) && style.bottomOffset >= 0 && style.bottomOffset <= 30;
    }

    function isValidLegacyPlayerSubtitleStyle(style) {
        return Number.isInteger(style?.fontSize) && style.fontSize >= 24 && style.fontSize <= 64
            && Number.isInteger(style?.marginY) && style.marginY >= 16 && style.marginY <= 160;
    }

    function playerSubtitlePosition(style = playerSubtitleStyle) {
        return 100 - style.bottomOffset;
    }

    function loadDefaultPlayerSubtitleStyle() {
        try {
            const value = window.localStorage.getItem(defaultSubtitleStyleSettingKey);
            if (value !== null) {
                const parsed = JSON.parse(value);
                if (isValidPlayerSubtitleStyle(parsed)) return { ...parsed };
                console.error('MediaStation default subtitle style is invalid');
                return { ...playerSubtitleStyleDefaults };
            }
            const legacyValue = window.localStorage.getItem(legacyDefaultSubtitleStyleSettingKey);
            if (legacyValue === null) return { ...playerSubtitleStyleDefaults };
            const legacyStyle = JSON.parse(legacyValue);
            if (!isValidLegacyPlayerSubtitleStyle(legacyStyle)) {
                console.error('MediaStation legacy default subtitle style is invalid');
                return { ...playerSubtitleStyleDefaults };
            }
            const migrated = {
                fontSize: legacyStyle.fontSize,
                bottomOffset: playerSubtitleStyleDefaults.bottomOffset,
            };
            try {
                window.localStorage.setItem(defaultSubtitleStyleSettingKey, JSON.stringify(migrated));
            } catch (error) {
                console.error(`MediaStation migrated default subtitle style could not be saved: ${error}`);
            }
            return migrated;
        } catch (error) {
            console.error(`MediaStation default subtitle style could not be read: ${error}`);
        }
        return { ...playerSubtitleStyleDefaults };
    }

    function isValidDanmakuStyle(style) {
        return Number.isInteger(style?.fontSize) && style.fontSize >= 20 && style.fontSize <= 40
            && Number.isInteger(style?.opacity) && style.opacity >= 35 && style.opacity <= 100
            && Number.isInteger(style?.displayArea) && style.displayArea >= 30 && style.displayArea <= 100
            && Number.isInteger(style?.scrollSeconds) && style.scrollSeconds >= 6 && style.scrollSeconds <= 20
            && Number.isInteger(style?.fixedSeconds) && style.fixedSeconds >= 3 && style.fixedSeconds <= 8;
    }

    function migrateDanmakuStyle(style) {
        const fields = [
            ['fontSize', 20, 40],
            ['opacity', 35, 100],
            ['displayArea', 30, 100],
            ['scrollSeconds', 6, 20],
            ['fixedSeconds', 3, 8],
        ];
        if (!fields.every(([property]) => Number.isFinite(Number(style?.[property])))) return null;
        return Object.fromEntries(fields.map(([property, minimum, maximum]) => [
            property,
            Math.max(minimum, Math.min(maximum, Math.round(Number(style[property])))),
        ]));
    }

    function loadDanmakuStyle() {
        try {
            const value = window.localStorage.getItem(danmakuStyleSettingKey);
            if (value === null) return { ...danmakuStyleDefaults };
            const parsed = JSON.parse(value);
            if (isValidDanmakuStyle(parsed)) return { ...parsed };
            const migrated = migrateDanmakuStyle(parsed);
            if (migrated) {
                window.localStorage.setItem(danmakuStyleSettingKey, JSON.stringify(migrated));
                return migrated;
            }
            console.error('MediaStation danmaku style is invalid');
        } catch (error) {
            console.error(`MediaStation danmaku style could not be read: ${error}`);
        }
        return { ...danmakuStyleDefaults };
    }

    function loadDanmakuEnabled() {
        try {
            const value = window.localStorage.getItem(danmakuEnabledSettingKey);
            if (value === null) return true;
            if (value === 'true') return true;
            if (value === 'false') return false;
            console.error('MediaStation danmaku switch is invalid');
        } catch (error) {
            console.error(`MediaStation danmaku switch could not be read: ${error}`);
        }
        return true;
    }

    function pushDanmakuEnabled() {
        if (window.jmpNative?.playerSetDanmaku) {
            window.jmpNative.playerSetDanmaku(danmakuEnabled);
        }
    }

    function pushDanmakuStyle() {
        if (window.jmpNative?.playerSetDanmakuStyle) {
            window.jmpNative.playerSetDanmakuStyle(
                danmakuStyle.fontSize,
                danmakuStyle.opacity,
                danmakuStyle.scrollSeconds,
                danmakuStyle.fixedSeconds,
                danmakuStyle.displayArea,
            );
        }
    }

    function refreshDanmakuStyleControls() {
        for (const definition of danmakuStyleControlDefinitions) {
            const input = byId(definition.id);
            const output = byId(definition.outputId);
            const value = danmakuStyle[definition.property];
            input.value = String(value);
            input.style.setProperty('--player-danmaku-progress', `${(value - definition.min) / (definition.max - definition.min) * 100}%`);
            output.value = `${value}${definition.suffix}`;
        }
    }

    function applyDanmakuStyle(style, persist = false) {
        const normalized = {
            fontSize: Math.round(Number(style?.fontSize)),
            opacity: Math.round(Number(style?.opacity)),
            displayArea: Math.round(Number(style?.displayArea)),
            scrollSeconds: Math.round(Number(style?.scrollSeconds)),
            fixedSeconds: Math.round(Number(style?.fixedSeconds)),
        };
        if (!isValidDanmakuStyle(normalized)) {
            throw new RangeError('Unsupported danmaku style');
        }
        danmakuStyle = normalized;
        danmakuRenderer.setStyle(normalized);
        if (persist) {
            try {
                window.localStorage.setItem(danmakuStyleSettingKey, JSON.stringify(normalized));
            } catch (error) {
                console.error(`MediaStation danmaku style could not be saved: ${error}`);
                showToast('弹幕样式保存失败');
            }
        }
        pushDanmakuStyle();
        refreshDanmakuStyleControls();
    }

    function applyDanmakuEnabled(enabled, persist = false) {
        danmakuEnabled = enabled === true;
        danmakuRenderer.setEnabled(danmakuEnabled);
        if (persist) {
            try {
                window.localStorage.setItem(danmakuEnabledSettingKey, String(danmakuEnabled));
            } catch (error) {
                console.error(`MediaStation danmaku switch could not be saved: ${error}`);
                showToast('弹幕开关保存失败');
            }
        }
        pushDanmakuEnabled();
        refreshPlayerTools();
    }

    function refreshDefaultSubtitleStyleControls() {
        const fontSize = byId('settings-subtitle-font-size');
        const bottomOffset = byId('settings-subtitle-bottom-offset');
        fontSize.value = String(defaultPlayerSubtitleStyle.fontSize);
        bottomOffset.value = String(defaultPlayerSubtitleStyle.bottomOffset);
        fontSize.style.setProperty('--setting-range-progress', `${(defaultPlayerSubtitleStyle.fontSize - 24) / 40 * 100}%`);
        bottomOffset.style.setProperty('--setting-range-progress', `${defaultPlayerSubtitleStyle.bottomOffset / 30 * 100}%`);
        byId('settings-subtitle-font-size-value').value = String(defaultPlayerSubtitleStyle.fontSize);
        byId('settings-subtitle-bottom-offset-value').value = `${defaultPlayerSubtitleStyle.bottomOffset}%`;
        const preview = byId('settings-subtitle-preview-text');
        preview.style.fontSize = `${defaultPlayerSubtitleStyle.fontSize * .65}px`;
        preview.style.bottom = `${defaultPlayerSubtitleStyle.bottomOffset}%`;
    }

    function applyDefaultPlayerSubtitleStyle(style, persist = false) {
        const normalized = {
            fontSize: Math.round(Number(style?.fontSize)),
            bottomOffset: Math.round(Number(style?.bottomOffset)),
        };
        if (!isValidPlayerSubtitleStyle(normalized)) {
            throw new RangeError('Unsupported default subtitle style');
        }
        defaultPlayerSubtitleStyle = normalized;
        if (persist) {
            try {
                window.localStorage.setItem(defaultSubtitleStyleSettingKey, JSON.stringify(normalized));
            } catch (error) {
                console.error(`MediaStation default subtitle style could not be saved: ${error}`);
                showToast('默认字幕样式保存失败');
            }
        }
        if (player && !playerSubtitleStyleCustomized) {
            applyPlayerSubtitleStyle(normalized, false, false);
        }
        refreshDefaultSubtitleStyleControls();
    }

    function isValidSeriesPlaybackSetting(setting) {
        return typeof setting?.autoNext === 'boolean'
            && Number.isInteger(setting?.introSkipSeconds)
            && setting.introSkipSeconds >= 0
            && setting.introSkipSeconds <= 600
            && Number.isInteger(setting?.outroSkipSeconds)
            && setting.outroSkipSeconds >= 0
            && setting.outroSkipSeconds <= 600;
    }

    function loadSeriesPlaybackSettings() {
        try {
            const value = window.localStorage.getItem(seriesPlaybackSettingsKey);
            if (value === null) return {};
            const parsed = JSON.parse(value);
            if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) return parsed;
            console.error('MediaStation series playback settings are invalid');
        } catch (error) {
            console.error(`MediaStation series playback settings could not be read: ${error}`);
        }
        return {};
    }

    function seriesPlaybackSettingFor(seriesId) {
        const setting = seriesId ? seriesPlaybackSettings[seriesId] : null;
        if (setting === undefined || setting === null) return { ...seriesPlaybackSettingDefaults };
        if (!isValidSeriesPlaybackSetting(setting)) {
            console.error(`MediaStation series playback setting is invalid for series: ${seriesId}`);
            return { ...seriesPlaybackSettingDefaults };
        }
        return { ...setting };
    }

    function persistSeriesPlaybackSetting(seriesId, setting) {
        if (!seriesId || !isValidSeriesPlaybackSetting(setting)) {
            throw new RangeError('Unsupported series playback setting');
        }
        const nextSettings = {
            ...seriesPlaybackSettings,
            [seriesId]: { ...setting },
        };
        try {
            window.localStorage.setItem(seriesPlaybackSettingsKey, JSON.stringify(nextSettings));
            seriesPlaybackSettings = nextSettings;
            return true;
        } catch (error) {
            console.error(`MediaStation series playback setting could not be saved: ${error}`);
            showToast('剧集连续播放设置保存失败');
            return false;
        }
    }

    function playerSettingsForScope(scopeId) {
        const settings = playerPlaybackSettings[scopeId];
        if (settings === undefined) {
            return {
                volume: 100,
                lastAudibleVolume: 100,
                subtitleStyle: null,
            };
        }
        const subtitleStyleValid = settings && typeof settings === 'object'
            && (settings.subtitleStyle === null || isValidPlayerSubtitleStyle(settings.subtitleStyle));
        const valid = settings && typeof settings === 'object'
            && Number.isInteger(settings.volume) && settings.volume >= 0 && settings.volume <= 100
            && Number.isInteger(settings.lastAudibleVolume) && settings.lastAudibleVolume >= 1 && settings.lastAudibleVolume <= 100
            && subtitleStyleValid;
        if (!valid) {
            console.error(`MediaStation playback settings are invalid for scope: ${scopeId}`);
            return {
                volume: 100,
                lastAudibleVolume: 100,
                subtitleStyle: null,
            };
        }
        return {
            volume: settings.volume,
            lastAudibleVolume: settings.lastAudibleVolume,
            subtitleStyle: settings.subtitleStyle ? { ...settings.subtitleStyle } : null,
        };
    }

    function friendlyError(error) {
        if (!(error instanceof Error)) return '请求失败';
        const codes = {
            invalid_credentials: '用户名或密码不正确',
            session_unavailable: '登录状态已失效',
            catalog_load_failed: '媒体内容加载失败',
            image_load_failed: '图片加载失败',
            preference_update_failed: '播放偏好保存失败',
            audio_track_unavailable: '所选音轨已不可用',
            subtitle_track_unavailable: '所选字幕已不可用',
            subtitle_style_invalid: '字幕样式无效',
            external_subtitle_download_failed: '字幕下载失败',
            unsupported_external_subtitle_format: '暂不支持该字幕格式',
            playback_changed: '当前播放项目已变化',
            playback_unavailable: '当前没有可切换轨道的播放项目',
            authentication_in_progress: '登录请求正在处理中',
            session_changed: '账号状态已变化，请重试',
            account_not_found: '所选账号已不存在，请刷新列表',
            invalid_account_id: '所选账号标识无效',
            account_identity_changed: '认证后的用户与原账号不一致，未修改原账号',
            saved_account_invalid: '所选账号凭据无效',
            credential_enumerate_failed: '无法读取 Windows 中保存的账号',
            credential_read_failed: '无法读取 Windows 中保存的账号凭据',
            credential_write_failed: '无法安全保存账号凭据',
            credential_delete_failed: '无法删除 Windows 中保存的账号凭据',
            credential_account_invalid: '已保存账号凭据无效',
            credential_account_target_invalid: '已保存账号标识无效',
            credential_account_mismatch: '已保存账号标识与凭据不一致',
            credential_persist_rollback_failed: '账号保存失败，且无法恢复之前的账号状态',
            credential_delete_rollback_failed: '账号删除失败，且无法恢复之前的账号状态',
            credential_active_mismatch: '保存的账号与当前会话不一致',
            updated_account_invalid: '修改后的账号无法配置',
            invalid_client_profile: '服务器客户端身份无效',
            invalid_proxy_mode: '服务器代理模式无效',
            invalid_connection_profile: '服务器连接配置无效',
            server_connection_mismatch: '该服务器已有账号使用其他连接配置',
            system_proxy_unavailable: '未检测到可用的 Windows 系统代理',
            playback_proxy_unavailable: '标准 Emby 播放需要可用的 Windows 系统代理',
            settings_write_failed: '无法保存当前服务器设置',
            frame_interpolation_runtime_unavailable: 'RIFE 插帧组件不可用',
            frame_interpolation_component_not_installed: '未安装插帧组件',
            frame_interpolation_component_not_published: '插帧组件包尚未发布，暂时不能在线安装',
            frame_interpolation_component_busy: '组件变更暂不可执行，请等待任务完成或退出其他播放器实例',
            frame_interpolation_component_invalid: '插帧组件校验或变更失败',
            frame_interpolation_nvidia_smi_unavailable: '无法读取 NVIDIA GPU 状态',
            frame_interpolation_nvidia_driver_unavailable: 'NVIDIA 驱动不可用',
            frame_interpolation_nvidia_output_invalid: 'NVIDIA GPU 信息无效',
            frame_interpolation_gpu_unsupported: '首版插帧仅支持 NVIDIA RTX',
            frame_interpolation_platform_unsupported: '当前系统不支持 RTX 插帧',
            frame_interpolation_not_initialized: 'RIFE 插帧组件尚未初始化',
            frame_interpolation_runtime_location_unavailable: '无法定位 RIFE 运行组件',
            frame_interpolation_manifest_missing: 'RIFE 运行清单缺失',
            frame_interpolation_manifest_invalid: 'RIFE 运行清单无效',
            frame_interpolation_runtime_component_missing: 'RIFE 运行组件不完整',
            frame_interpolation_runtime_component_corrupt: 'RIFE 运行组件校验失败',
            frame_interpolation_runtime_load_failed: 'RIFE 运行库加载失败',
            frame_interpolation_runtime_abi_mismatch: 'RIFE 运行库版本不匹配',
            frame_interpolation_engine_cache_mismatch: 'RIFE Engine 与当前显卡或驱动不匹配',
            frame_interpolation_engine_missing: '当前分辨率的 RIFE Engine 缺失',
            frame_interpolation_engine_corrupt: 'RIFE Engine 校验失败',
            frame_interpolation_engine_cache_unavailable: 'RIFE Engine 缓存不可用',
            frame_interpolation_engine_build_failed: 'RIFE Engine 生成失败',
            frame_interpolation_model_unavailable: '所选 RIFE 模型未安装',
            frame_interpolation_model_corrupt: '所选 RIFE 模型校验失败',
            frame_interpolation_model_invalid: '所选 RIFE 模型无效',
            frame_interpolation_engine_shape_unsupported: '当前分辨率尚无原生 RIFE Engine',
            frame_interpolation_runtime_path_invalid: 'RIFE 运行路径无效',
            frame_interpolation_d3d_compiler_unavailable: 'D3D11 着色器编译组件不可用',
            frame_interpolation_mode_invalid: 'RTX 插帧目标设置无效',
            frame_interpolation_request_invalid: 'RTX 插帧请求无效',
            frame_interpolation_operation_invalid: 'RTX 插帧操作不受支持',
            frame_interpolation_video_metadata_missing: '缺少插帧所需的视频信息',
            frame_interpolation_dimensions_unknown: '无法确认视频分辨率，已拒绝插帧',
            frame_interpolation_source_fps_invalid: '无法确认视频原始帧率，已拒绝插帧',
            frame_interpolation_target_not_higher: '插帧目标帧率必须高于原始帧率',
            frame_interpolation_display_fps_unknown: '无法读取当前显示器刷新率',
            frame_interpolation_display_refresh_insufficient: '目标帧率高于当前显示器刷新率',
            frame_interpolation_dimensions_unsupported: '该分辨率超出当前插帧链路限制',
            frame_interpolation_source_fps_unsupported: '当前仅支持 20 至 30 FPS 片源进行严格 2 倍插帧',
            frame_interpolation_hlg_not_validated: '当前版本尚未开放 HLG 插帧',
            frame_interpolation_hdr10_plus_unsupported: '当前版本不支持 HDR10+ 插帧',
            frame_interpolation_dolby_vision_unsupported: '当前版本不支持 Dolby Vision 插帧',
            frame_interpolation_dynamic_range_unknown: '无法确认视频动态范围，已拒绝插帧',
            frame_interpolation_hdr10_transfer_invalid: 'HDR10 缺少 PQ/ST2084 色彩元数据',
            frame_interpolation_color_space_unsupported: '视频色彩空间不支持插帧',
            frame_interpolation_color_range_unsupported: '视频色彩范围不支持插帧',
            frame_interpolation_filter_invalid: 'RIFE 滤镜参数无效',
            frame_interpolation_hwdec_invalid: 'RTX 插帧硬件解码参数无效',
            frame_interpolation_setting_save_failed: 'RTX 插帧设置保存失败',
            frame_interpolation_item_enabled_invalid: '影片插帧设置无效',
            frame_interpolation_item_setting_invalid: '无法保存该影片的插帧设置',
            frame_interpolation_state_mismatch: '播放器返回的 RTX 插帧状态与请求不一致',
            frame_interpolation_filter_inactive: 'RIFE 原生滤镜未生效',
            video_enhancement_filter_inactive: '画面增强原生滤镜未生效，播放已停止；请检查组件、GPU 和媒体格式',
            video_enhancement_format_unsupported: '画面增强支持 SDR、HDR10 和 HLG，暂不支持 HDR10+ 和 Dolby Vision',
        };
        return codes[error.code] || error.message || '请求失败';
    }

    function initials(name) {
        const value = String(name || 'M').trim();
        return (value[0] || 'M').toUpperCase();
    }

    function updateSessionUi() {
        const name = session?.userName || session?.userId || 'Media';
        const initial = initials(name);
        const baseUrl = session?.baseUrl || '';
        const serverName = serverLabel(baseUrl);
        byId('account-name').textContent = name;
        byId('account-server-name').textContent = serverName;
        byId('account-server-name').title = baseUrl;
        byId('account-open').title = [name, baseUrl].filter(Boolean).join(' · ');
        byId('account-avatar').textContent = initial;
        byId('drawer-avatar').textContent = initial;
        byId('drawer-user').textContent = name;
        byId('drawer-server').textContent = baseUrl;
    }

    function validConnectionProfile(value) {
        return Boolean(value
            && ['mediastation_windows', 'senplayer', 'infuse'].includes(value.clientProfile));
    }

    function defaultConnectionProfile() {
        return {
            clientProfile: 'mediastation_windows',
        };
    }

    function embyClientProfileInputs() {
        return document.querySelectorAll('input[name="emby-client-profile"]');
    }

    function selectedEmbyClientProfile() {
        const selected = document.querySelector('input[name="emby-client-profile"]:checked');
        if (!selected) throw new Error('请先选择 Emby 客户端身份。');
        return selected.value;
    }

    function setEmbyClientProfile(profile, disabled) {
        embyClientProfileInputs().forEach((input) => {
            input.checked = input.value === profile;
            input.disabled = disabled;
        });
    }

    function selectedLoginConnection() {
        return {
            clientProfile: selectedEmbyClientProfile(),
        };
    }

    function setLoginConnection(source, locked) {
        const connection = source && validConnectionProfile(source)
            ? source
            : defaultConnectionProfile();
        loginConnectionLocked = locked;
        setEmbyClientProfile(connection.clientProfile, locked);
    }

    function setLoginLoading(loading) {
        const form = byId('login-form');
        form.classList.toggle('is-loading', loading);
        if (loading) form.setAttribute('aria-busy', 'true');
        else form.removeAttribute('aria-busy');
        byId('login-submit').disabled = loading;
        byId('login-cancel').disabled = loading;
        byId('server-url').disabled = loading;
        byId('username').disabled = loading;
        byId('password').disabled = loading;
        embyClientProfileInputs().forEach((input) => {
            input.disabled = loading || loginConnectionLocked;
        });
        byId('login-submit-label').textContent = loading
            ? loginMode === 'update' ? '正在保存...' : '正在登录...'
            : loginMode === 'update' ? '保存修改' : '登录';
    }

    function setPlayerMode(enabled) {
        document.documentElement.classList.toggle('player-mode', enabled);
        document.body.classList.toggle('player-mode', enabled);
        const shellColor = getComputedStyle(document.documentElement).getPropertyValue('--bg').trim();
        const windowColor = enabled ? '#000000' : shellColor;
        if (windowColor && window.jmpNative?.themeColor) window.jmpNative.themeColor(windowColor);
    }

    function showLogin(
        baseUrl = '',
        message = '',
        canCancel = false,
        mode = 'login',
        account = null,
        connection = null,
    ) {
        loginMode = mode;
        loginAccount = account;
        loginCanCancel = canCancel;
        loginAccountBaseUrl = baseUrl;
        splash.classList.add('hidden');
        appShell.classList.add('hidden');
        playerView.classList.add('hidden');
        setPlayerMode(false);
        loginView.classList.remove('hidden');
        loginView.setAttribute('aria-labelledby', 'login-title');
        byId('login-account-panel').classList.add('hidden');
        byId('login-form').classList.remove('hidden');
        byId('server-url').value = baseUrl;
        byId('server-url').readOnly = mode === 'update' || mode === 'server-user';
        setLoginConnection(connection || account, mode === 'update' || mode === 'server-user');
        byId('username').value = mode === 'update' ? account?.userName || '' : '';
        byId('password').value = '';
        byId('login-cancel').classList.toggle('hidden', !canCancel);
        const backLabel = session ? '返回账号管理' : '返回账号列表';
        byId('login-cancel').title = backLabel;
        byId('login-cancel').setAttribute('aria-label', backLabel);
        byId('login-title').textContent = mode === 'update'
            ? '修改账号'
            : mode === 'server-user' ? '添加服务器用户'
                : mode === 'add' ? '添加账号' : '登录媒体服务器';
        setLoginLoading(false);
        byId('login-error').textContent = message;
        window.setTimeout(() => byId(baseUrl ? 'username' : 'server-url').focus(), 0);
    }

    async function showAccountGateway(baseUrl = '', connection = null, message = '') {
        loginCanCancel = false;
        loginReturnFocus = null;
        loginMode = 'login';
        loginAccount = null;
        loginAccountBaseUrl = baseUrl;
        splash.classList.add('hidden');
        appShell.classList.add('hidden');
        playerView.classList.add('hidden');
        setPlayerMode(false);
        loginView.classList.remove('hidden');
        loginView.setAttribute('aria-labelledby', 'login-account-title');
        byId('login-form').classList.add('hidden');
        byId('login-account-panel').classList.remove('hidden');
        await loadSavedAccounts('login', baseUrl, connection);
        if (message && !byId('login-account-panel').classList.contains('hidden')) showToast(message);
        else if (message) byId('login-error').textContent = message;
    }

    async function showApp(account) {
        session = account;
        loginCanCancel = false;
        loginReturnFocus = null;
        loginMode = 'add';
        loginAccount = null;
        updateSessionUi();
        splash.classList.add('hidden');
        loginView.classList.add('hidden');
        appShell.classList.remove('hidden');
        byId('login-cancel').classList.add('hidden');
        refreshUpdateControls();
        scheduleAutomaticUpdateCheck();
        refreshImageCacheStatus();
        refreshFrameInterpolationStatus();
        await loadHome(true);
    }

    function resetCatalogState() {
        invalidateHomeRefresh();
        homeRefreshFailures = 0;
        homeVerificationPending = false;
        stopHeroCarousel();
        window.clearTimeout(imageStatsTimer);
        imageStatsTimer = 0;
        window.clearTimeout(interpolationComponentTimer);
        interpolationComponentTimer = 0;
        interpolationComponentTracking = false;
        homeData = null;
        currentView = null;
        history.length = 0;
        heroRevision += 1;
        appBackdropRevision += 1;
        window.clearTimeout(heroSlideTimer);
        heroSlideTimer = 0;
        stopSmoothScroll(content);
        clearAppBackdrop(appBackdropRevision);
        imageCache.clear();
        heroArtworkCache.clear();
        imagePending.clear();
        libraryCache.clear();
        libraryBrowsePreferences.clear();
        byId('sidebar-libraries').replaceChildren();
        delete byId('sidebar-libraries').dataset.identity;
        seriesDetailCache.clear();
        libraryPageObserver.disconnect();
        personPageObserver.disconnect();
        while (imageQueue.length) {
            imageQueue.shift().reject(new Error('账号状态已变化'));
        }
        updateImageCacheStatus();
    }

    function beginAddAccount(baseUrl = '', lockServer = false, trigger = null, server = null) {
        loginReturnFocus = trigger || (session ? byId('account-open') : byId('login-add-account'));
        closeDrawer(false);
        const selectedBaseUrl = baseUrl || session?.baseUrl || '';
        const connection = server
            || (session?.baseUrl === selectedBaseUrl && validConnectionProfile(session) ? session : null);
        showLogin(
            selectedBaseUrl,
            '',
            true,
            lockServer ? 'server-user' : 'add',
            null,
            connection,
        );
    }

    function beginUpdateAccount(account, trigger = null) {
        if (!account?.accountId) return;
        loginReturnFocus = trigger || byId('account-open');
        closeDrawer(false);
        showLogin(account.baseUrl, '', true, 'update', account);
    }

    function cancelAccountChange() {
        if (!loginCanCancel) return;
        loginCanCancel = false;
        loginMode = 'add';
        loginAccount = null;
        byId('server-url').readOnly = false;
        setLoginConnection(null, false);
        byId('login-error').textContent = '';
        byId('password').value = '';
        byId('login-cancel').classList.add('hidden');
        const target = loginReturnFocus;
        loginReturnFocus = null;
        if (session) {
            loginView.classList.add('hidden');
            appShell.classList.remove('hidden');
            requestAnimationFrame(() => focusElement(target || byId('account-open')));
            return;
        }
        void showAccountGateway(loginAccountBaseUrl);
    }

    async function initialize() {
        await new Promise((resolve) => window.setTimeout(resolve, 280));
        try {
            const status = await nativeRequest('mediaStationSessionStatus', 'session_status');
            if (status.configured) {
                await showApp(status);
            } else {
                await showAccountGateway(status.baseUrl || '');
            }
        } catch (error) {
            await showAccountGateway('', null, friendlyError(error));
        }
    }

    byId('login-form').addEventListener('submit', async (event) => {
        event.preventDefault();
        const button = byId('login-submit');
        const errorText = byId('login-error');
        const mode = loginMode;
        const account = loginAccount;
        if (button.disabled) return;
        setLoginLoading(true);
        errorText.textContent = '';
        try {
            const connection = selectedLoginConnection();
            const status = mode === 'update'
                ? await nativeRequest('mediaStationUpdateAccount', 'update_account', [
                    account.accountId,
                    byId('username').value.trim(),
                    byId('password').value,
                    connection.clientProfile,
                ])
                : await nativeRequest('mediaStationAuthenticate', 'authenticate', [
                    byId('server-url').value.trim(),
                    byId('username').value.trim(),
                    byId('password').value,
                    connection.clientProfile,
                ]);
            byId('password').value = '';
            const updatedInactiveAccount = mode === 'update' && !isCurrentAccount(account);
            if (updatedInactiveAccount) {
                if (session) returnToAccountDrawer();
                else await showAccountGateway(status.baseUrl || account.baseUrl, status);
                showToast('账号已更新');
            } else {
                resetCatalogState();
                await showApp(status);
            }
        } catch (error) {
            errorText.textContent = friendlyError(error);
        } finally {
            setLoginLoading(false);
        }
    });

    function captureView() {
        if (!currentView) return null;
        const rows = {};
        content.querySelectorAll('.media-row[data-row-key]').forEach((row) => {
            rows[row.dataset.rowKey] = row.scrollLeft;
        });
        const active = document.activeElement;
        const focusKey = active instanceof Element
            ? active.closest('[data-focus-key]')?.dataset.focusKey || ''
            : '';
        if (currentView.kind === 'library') libraryBrowsePreferences.set(currentView.data.library.id, {
            filter: { ...currentView.data.filter }, viewMode: currentView.data.viewMode || 'grid', scrollTop: content.scrollTop,
            filterExpanded: { ...currentView.data.filterExpanded },
        });
        return { ...currentView, scrollTop: content.scrollTop, rows, focusKey };
    }

    function setCurrentView(view, pushHistory = true, capturedOverride) {
        const previousView = currentView;
        const captured = capturedOverride === undefined ? captureView() : capturedOverride;
        if (pushHistory && captured) history.push(captured);
        currentView = { ...view, scrollTop: 0, rows: {}, focusKey: '' };
        content.scrollTop = 0;
        renderCurrentView(captured ? 'forward' : '');
        refreshHomeAfterEntry(previousView);
        refreshFavoritesAfterEntry(previousView);
    }

    function beginLoadingView(kind, captured, render) {
        if (captured) history.push(captured);
        const loadingView = { kind, data: null, scrollTop: 0, rows: {}, focusKey: '' };
        currentView = loadingView;
        content.scrollTop = 0;
        appBackdropRevision += 1;
        updateTopbarState();
        updateNavState();
        render();
        playContentTransition('forward');
        return loadingView;
    }

    function restoreLoadingSource(loadingView, captured) {
        if (currentView !== loadingView) return false;
        const previousView = currentView;
        const previous = history.pop();
        if (previous) {
            currentView = previous;
            renderCurrentView('back');
            restoreViewState(previous);
        } else if (captured) {
            currentView = captured;
            renderCurrentView('back');
            restoreViewState(captured);
        }
        refreshHomeAfterEntry(previousView);
        refreshFavoritesAfterEntry(previousView);
        return true;
    }

    function updateStatusText(status, payload) {
        if (status === 'checking') return '正在检查更新...';
        if (status === 'available') {
            const packageLabel = payload.packageKind === 'portable' ? '便携版' : '安装版';
            const assetBytes = Number(payload.assetBytes);
            const size = Number.isFinite(assetBytes) && assetBytes > 0 ? formatBytes(assetBytes) : '';
            return `发现新版本 ${displayAppVersion(payload.version)} · ${packageLabel}${size ? ` · ${size}` : ''}`;
        }
        if (status === 'downloading') {
            const percent = Number.isFinite(Number(payload.percent)) ? Number(payload.percent) : 0;
            const downloaded = formatBytes(payload.downloadedBytes);
            const total = formatBytes(payload.totalBytes);
            const progress = downloaded && total ? ` · ${downloaded} / ${total}` : '';
            const source = payload.source ? ` · ${payload.source}` : '';
            return `正在下载更新 ${Math.max(0, Math.min(100, percent))}%${progress}${source}`;
        }
        if (status === 'verifying') return '正在校验更新包...';
        if (status === 'ready') return `${displayAppVersion(payload.version)} 已下载并校验完成，可以应用更新`;
        if (status === 'installing') return payload.packageKind === 'portable'
            ? '正在应用便携版更新，应用即将关闭...'
            : '正在启动安装程序，应用即将关闭...';
        if (status === 'up_to_date') return '当前已是最新版本';
        if (status === 'error') return payload.message || '更新检查失败';
        return '尚未检查更新';
    }

    function displayAppVersion(version) {
        const value = String(version || '').trim();
        if (!value) return '未知版本';
        return value.toLowerCase().startsWith('v') ? value : `v${value}`;
    }

    function refreshUpdateControls() {
        const versionNode = byId('settings-app-version');
        const statusNode = byId('settings-app-update-status');
        const checkButton = byId('settings-check-app-update');
        const downloadButton = byId('settings-download-app-update');
        const installButton = byId('settings-install-app-update');
        const confirm = byId('settings-confirm-app-update');
        if (!versionNode || !statusNode || !checkButton || !downloadButton || !installButton || !confirm) return;
        const { status, payload } = appUpdateState;
        versionNode.textContent = displayAppVersion(window.jmpInfo?.version);
        statusNode.textContent = updateStatusText(status, payload);
        statusNode.dataset.state = status === 'error'
            ? 'error'
            : (status === 'ready' || status === 'up_to_date' ? 'ready' : '');
        const downloading = status === 'downloading';
        const applying = status === 'verifying' || status === 'installing';
        const canResumeDownload = status === 'error' && payload.canResume === true;
        checkButton.disabled = status === 'checking';
        checkButton.dataset.busy = String(status === 'checking');
        checkButton.textContent = status === 'checking'
            ? '正在检查...'
            : status === 'error' ? '重试检查' : status === 'idle' ? '检查更新' : '重新检查';
        checkButton.classList.toggle('hidden', downloading || applying || canResumeDownload);
        downloadButton.disabled = downloading;
        downloadButton.dataset.busy = String(downloading);
        downloadButton.textContent = downloading
            ? `正在下载 ${Math.max(0, Math.min(100, Number(payload.percent) || 0))}%`
            : canResumeDownload
                ? `继续下载 ${displayAppVersion(payload.version)}`
            : `下载 ${displayAppVersion(payload.version)}`;
        downloadButton.classList.toggle('hidden', status !== 'available' && !downloading && !canResumeDownload);
        installButton.disabled = applying;
        installButton.dataset.busy = String(applying);
        installButton.textContent = status === 'verifying'
            ? '正在校验...'
            : status === 'installing' ? '正在应用更新...' : `应用 ${displayAppVersion(payload.version)}`;
        installButton.classList.toggle('hidden', status !== 'ready' && !applying);
        if (status !== 'ready') confirm.classList.add('hidden');
    }

    function requestUpdateCheck() {
        window.clearTimeout(automaticUpdateTimer);
        automaticUpdateTimer = 0;
        if (!window.jmpNative?.updateCheck) {
            appUpdateState = { status: 'error', payload: { message: '当前版本不支持自动更新' } };
            refreshUpdateControls();
            return;
        }
        appUpdateState = { status: 'checking', payload: {} };
        refreshUpdateControls();
        window.jmpNative.updateCheck();
    }

    function scheduleAutomaticUpdateCheck() {
        window.clearTimeout(automaticUpdateTimer);
        automaticUpdateTimer = 0;
        if (!autoUpdateCheckEnabled || !window.jmpNative?.updateCheck) return;
        automaticUpdateTimer = window.setTimeout(() => {
            automaticUpdateTimer = 0;
            if (appUpdateState.status === 'idle' || appUpdateState.status === 'up_to_date') {
                requestUpdateCheck();
            }
        }, 5000);
    }

    window._onAppUpdateStatus = (status, payloadJson) => {
        let payload = {};
        try { payload = JSON.parse(payloadJson || '{}'); } catch { payload = {}; }
        appUpdateState = { status, payload: { ...appUpdateState.payload, ...payload } };
        refreshUpdateControls();
        if (status === 'available' && payload.version && payload.version !== updateNotificationVersion) {
            updateNotificationVersion = payload.version;
            showToast(`发现新版本 ${payload.version}，请在设置中下载`);
        }
    };

    function playContentTransition(direction) {
        window.clearTimeout(contentTransitionTimer);
        content.classList.remove('content-enter-forward', 'content-enter-back');
        if (!direction) return;
        void content.offsetWidth;
        content.classList.add(`content-enter-${direction}`);
        contentTransitionTimer = window.setTimeout(() => {
            content.classList.remove('content-enter-forward', 'content-enter-back');
        }, 300);
    }

    function focusTargetForView(view) {
        if (view.focusKey) {
            const target = [...document.querySelectorAll('[data-focus-key]')]
                .find((node) => node.dataset.focusKey === view.focusKey);
            if (target && !target.disabled && target.getClientRects().length) return target;
            console.warn(`无法恢复焦点目标：${view.focusKey}`);
        }
        return [...content.querySelectorAll('.search-field input, .back-button, .detail-back, .person-card, .media-card, button:not(:disabled)')]
            .find((node) => !node.disabled && node.getClientRects().length)
            || content;
    }

    function restoreViewState(view, { restoreFocus = true } = {}) {
        requestAnimationFrame(() => {
            content.scrollTop = view.scrollTop || 0;
            content.querySelectorAll('.media-row[data-row-key]').forEach((row) => {
                row.scrollLeft = view.rows?.[row.dataset.rowKey] || 0;
            });
            requestAnimationFrame(() => {
                if (restoreFocus) {
                    const target = focusTargetForView(view);
                    if (target === content) {
                        content.focus({ preventScroll: true });
                    } else {
                        focusElement(target, true);
                    }
                }
                content.scrollTop = view.scrollTop || 0;
            });
        });
    }

    function goBack() {
        if (!byId('source-scrim').classList.contains('hidden')) {
            closeSourcePicker();
            return;
        }
        if (!byId('overview-scrim').classList.contains('hidden')) {
            closeOverview();
            return;
        }
        if (currentDrawer) {
            closeDrawer();
            return;
        }
        const previousView = currentView;
        const previous = history.pop();
        if (!previous) {
            if (currentView?.kind !== 'home' && homeData) setCurrentView({ kind: 'home', data: homeData }, false);
            return;
        }
        currentView = previous;
        renderCurrentView('back');
        restoreViewState(previous);
        refreshHomeAfterEntry(previousView);
        refreshFavoritesAfterEntry(previousView);
    }

    function renderCurrentView(transition = '') {
        parkSettings();
        stopSmoothScroll(content);
        if (currentView?.kind !== 'home') stopHeroCarousel();
        appBackdropRevision += 1;
        updateTopbarState();
        updateNavState();
        switch (currentView?.kind) {
            case 'home': renderHome(currentView.data); break;
            case 'resume': renderResume(currentView.data); break;
            case 'favorites': renderFavorites(currentView.data); break;
            case 'settings': renderSettings(); break;
            case 'components': renderComponents(); break;
            case 'libraries': renderLibraries(currentView.data); break;
            case 'library': renderLibrary(currentView.data); break;
            case 'detail': renderDetail(currentView.data); break;
            case 'person': renderPerson(currentView.data); break;
            case 'search': renderSearch(currentView.data); break;
            default: renderEmpty('没有可显示的内容');
        }
        playContentTransition(transition);
    }

    function parkSettings() {
        const components = byId('components-page');
        if (content.contains(components)) { components.classList.add('hidden'); document.body.append(components); }
        const settings = byId('settings-drawer');
        if (content.contains(settings)) {
            settings.classList.remove('settings-page', 'open');
            settings.setAttribute('aria-hidden', 'true');
            settings.setAttribute('role', 'dialog');
            settings.setAttribute('aria-modal', 'true');
            document.body.append(settings);
        }
    }

    function replaceContent(...nodes) {
        parkSettings();
        content.replaceChildren(...nodes);
    }

    function updateSidebarLibraries() {
        const host = byId('sidebar-libraries'); const libraries = homeData?.libraries || [];
        const identity = JSON.stringify(libraries.map((library) => [library.id, library.title, library.childCount]));
        if (host.dataset.identity !== identity) {
            host.dataset.identity = identity; host.replaceChildren();
            libraries.forEach((library) => {
                const button = element('button', 'nav-command sidebar-library'); button.type = 'button';
                button.dataset.libraryId = library.id; button.dataset.focusKey = 'library:' + library.id; button.title = library.title;
                const icon = strokeIcon(library.collectionType === 'movies' ? 'film' : 'tv');
                button.append(icon, element('span', '', library.title));
                if (Number.isInteger(library.childCount) && library.childCount > 0) button.append(element('small', '', library.childCount));
                button.addEventListener('click', () => openLibrary(library)); host.append(button);
            });
        }
        [...host.children].forEach((button) => {
            const selected = currentView?.kind === 'library' && currentView.data.library.id === button.dataset.libraryId;
            button.classList.toggle('active', selected);
            if (selected) button.setAttribute('aria-current', 'page'); else button.removeAttribute('aria-current');
        });
    }

    function updateNavState() {
        updateSidebarLibraries(); const kind = currentView?.kind;
        byId('view-title').textContent = kind === 'library' ? currentView.data.library.title
            : kind === 'detail' ? cardTitle(currentView.data.item, true)
            : ({ home: '首页', libraries: '媒体库', resume: '继续观看', favorites: '收藏', settings: '设置', components: '组件管理', search: '搜索', person: '演职人员',
                'library-loading': '媒体库', 'detail-loading': '作品详情' })[kind] || '你的媒体空间';
        byId('nav-home').classList.toggle('active', currentView?.kind === 'home');
        byId('nav-library').classList.toggle('active', ['libraries', 'library'].includes(currentView?.kind));
        byId('nav-resume').classList.toggle('active', kind === 'resume');
        byId('nav-favorites').classList.toggle('active', kind === 'favorites');
        byId('nav-resume-count').textContent = homeData?.resume?.length || '';
        byId('settings-open').classList.toggle('active', kind === 'settings');
        byId('components-open').classList.toggle('active', kind === 'components');
    }

    function updateTopbarState() {
        const atTop = content.scrollTop <= 20;
        const detailAtTop = atTop && ['detail', 'detail-loading'].includes(currentView?.kind);
        topbar.classList.toggle('detail-transparent', detailAtTop);
        topbar.classList.toggle('scrolled', !atTop);
        byId('content-to-top').classList.toggle('hidden', content.scrollTop < 520);
    }

    function renderLoading() {
        replaceContent();
        const wrap = element('div', 'loading-grid');
        for (let index = 0; index < 12; index += 1) {
            const card = element('div', 'placeholder-card');
            const copy = element('div', 'placeholder-copy');
            copy.append(element('span', 'placeholder-title'), element('span', 'placeholder-subtitle'));
            card.append(element('div', 'placeholder-art'), copy);
            card.setAttribute('aria-hidden', 'true');
            wrap.append(card);
        }
        content.append(wrap);
    }

    function renderEmpty(message) {
        replaceContent(element('div', 'empty-state', message));
    }

    function invalidateHomeRefresh() {
        window.clearTimeout(homeRefreshTimer);
        homeRefreshTimer = 0;
        homeRefreshGeneration += 1;
    }

    function refreshHomeAfterEntry(previousView) {
        if (!previousView || previousView.kind === 'home' || currentView?.kind !== 'home') return;
        scheduleHomeRefresh(0);
    }

    function refreshFavoritesAfterEntry(previousView) {
        if (currentView?.kind !== 'favorites' || previousView?.data === currentView.data) return;
        void loadFavorites(currentView.data, { refresh: true });
    }

    async function loadHome(replaceHistory = false) {
        if (!homeData) renderLoading();
        try {
            homeData = await nativeRequest('mediaStationCatalog', 'home', ['home', JSON.stringify({ refresh: false })]);
            appShell.dataset.initialHomeCache = homeData.cache?.status || 'unknown';
            history.length = replaceHistory ? 0 : history.length;
            setCurrentView({ kind: 'home', data: homeData }, !replaceHistory && currentView !== null);
            if (homeData.cache?.writeFailed) {
                console.error('首页数据已加载，但首页快照写入失败');
            }
            if (homeData.cache?.status === 'hit') scheduleHomeRefresh();
        } catch (error) {
            renderEmpty(friendlyError(error));
            showToast(friendlyError(error));
        }
    }

    function scheduleHomeRefresh(delayMs = homeRefreshDelayMs) {
        window.clearTimeout(homeRefreshTimer);
        const generation = ++homeRefreshGeneration;
        const expectedSession = session;
        homeRefreshTimer = window.setTimeout(async () => {
            homeRefreshTimer = 0;
            try {
                const refreshed = await nativeRequest('mediaStationCatalog', 'home', [
                    'home',
                    JSON.stringify({ refresh: true }),
                ], 60000);
                if (generation !== homeRefreshGeneration || session !== expectedSession) return;
                if (refreshed.cache?.writeFailed) {
                    console.error('首页同步完成，但首页快照写入失败');
                }
                homeData = refreshed;
                homeRefreshFailures = 0;
                if (currentView?.kind === 'home') {
                    const saved = captureView();
                    currentView = { ...currentView, data: homeData };
                    renderHome(homeData);
                    if (saved) restoreViewState(saved, { restoreFocus: Boolean(saved.focusKey) });
                }
            } catch (error) {
                if (generation !== homeRefreshGeneration || session !== expectedSession) return;
                // A background refresh failure should not disturb the user;
                // log it and retry a few times before giving up.
                homeRefreshFailures += 1;
                console.error(`首页后台同步失败：${friendlyError(error)}`);
                if (homeRefreshFailures < 3) {
                    window.setTimeout(() => scheduleHomeRefresh(), 4000);
                }
            }
        }, delayMs);
    }

    function imageWidthFor(ref, landscape, hero = false) {
        if (hero) return Math.min(2000, Math.max(1280, Math.ceil(content.clientWidth * Math.min(window.devicePixelRatio || 1, 2))));
        if (ref?.type === 'backdrop' || landscape) return 640;
        return 360;
    }

    function observeImage(img, ref, width) {
        if (!ref) return;
        img.dataset.imageRef = JSON.stringify(ref);
        img.dataset.imageKey = ref.key;
        img.dataset.imageWidth = String(width);
        const cached = imageCache.get(imageMemoryKey(ref, width));
        if (cached) {
            setImageSource(img, cached);
        } else {
            imageObserver.observe(img);
        }
    }

    function setImageSource(img, src) {
        img.classList.remove('image-error', 'image-ready');
        img.onload = () => img.classList.add('image-ready');
        img.onerror = () => img.classList.add('image-error');
        img.src = src;
        if (img.complete && img.naturalWidth > 0) img.classList.add('image-ready');
    }

    function setAppBackdrop(src, revision = appBackdropRevision) {
        if (!src || revision !== appBackdropRevision || appBackdropLayers.length !== 2) return;
        if (src === appBackdropSrc) return;
        const activeIndex = Number(appBackdrop.dataset.activeLayer ?? -1);
        const nextIndex = activeIndex === 0 ? 1 : 0;
        const next = appBackdropLayers[nextIndex];
        const previous = appBackdropLayers[activeIndex];
        next.dataset.revision = String(revision);
        next.style.backgroundImage = `url("${src}")`;
        void next.offsetWidth;
        next.classList.add('active');
        previous?.classList.remove('active');
        appBackdrop.dataset.activeLayer = String(nextIndex);
        appBackdropSrc = src;
        appBackdrop.classList.add('ready');
        window.setTimeout(() => {
            if (next.dataset.revision !== String(revision) || previous?.classList.contains('active')) return;
            if (previous) previous.style.backgroundImage = '';
        }, 1300);
    }

    function clearAppBackdrop(revision = appBackdropRevision) {
        if (revision !== appBackdropRevision) return;
        appBackdropLayers.forEach((layer) => layer.classList.remove('active'));
        appBackdrop.classList.remove('ready');
        appBackdropSrc = '';
        delete appBackdrop.dataset.activeLayer;
    }

    function loadViewBackdrop(ref, revision = appBackdropRevision, expectedView = currentView) {
        if (!ref) {
            clearAppBackdrop(revision);
            return;
        }
        requestImage(ref, imageWidthFor(ref, true, true), { background: true }).then((src) => {
            if (revision !== appBackdropRevision || currentView !== expectedView) return;
            setAppBackdrop(src, revision);
        }).catch((error) => {
            if (revision !== appBackdropRevision || currentView !== expectedView) return;
            console.error(`环境背景加载失败：${friendlyError(error)}`);
            clearAppBackdrop(revision);
        });
    }

    function loadObservedImage(img) {
        const raw = img.dataset.imageRef;
        if (!raw) return;
        let ref;
        try { ref = JSON.parse(raw); } catch { return; }
        requestImage(ref, Number(img.dataset.imageWidth) || 360)
            .then((src) => { if (img.isConnected) setImageSource(img, src); })
            .catch((error) => {
                console.error(`Image request failed: ${friendlyError(error)}`);
                img.classList.add('image-error');
            });
    }

    function requestImage(ref, width, { background = false } = {}) {
        const memoryKey = imageMemoryKey(ref, width);
        const cached = imageCache.get(memoryKey);
        if (cached) return Promise.resolve(cached);
        const active = imagePending.get(memoryKey);
        if (active) {
            if (!background) {
                const queued = imageQueue.find((task) => task.memoryKey === memoryKey);
                if (queued) { queued.background = false; pumpImages(); }
            }
            return active;
        }
        const request = new Promise((resolve, reject) => {
            imageQueue.push({ ref, width, memoryKey, background, resolve, reject });
            pumpImages();
        });
        imagePending.set(memoryKey, request);
        request.then(
            () => imagePending.delete(memoryKey),
            () => imagePending.delete(memoryKey),
        );
        return request;
    }

    function imageMemoryKey(ref, width) {
        return `${ref.key}@${Math.round(width)}`;
    }

    function pumpImages() {
        while (imageActive < 2 && imageQueue.length > 0) {
            // Keep one native request slot available for visible covers while large artwork loads.
            let index = imageQueue.findIndex((task) => !task.background);
            if (index < 0) {
                if (imageBackgroundActive >= 1) break;
                index = 0;
            }
            const [task] = imageQueue.splice(index, 1);
            imageActive += 1;
            if (task.background) imageBackgroundActive += 1;
            nativeRequest('mediaStationImage', 'image', [JSON.stringify(task.ref), Math.round(task.width)], 40000)
                .then((payload) => {
                    if (payload.key !== task.ref.key || typeof payload.dataUrl !== 'string' || !payload.dataUrl.startsWith('data:image/')) {
                        throw new Error('图片响应校验失败');
                    }
                    if (payload.cacheWriteFailed) {
                        console.error(`图片已加载，但磁盘缓存写入失败：${task.ref.key}`);
                    }
                    imageCache.set(task.memoryKey, payload.dataUrl);
                    updateImageCacheStatus();
                    task.resolve(payload.dataUrl);
                })
                .catch(task.reject)
                .finally(() => {
                    imageActive -= 1;
                    if (task.background) imageBackgroundActive -= 1;
                    pumpImages();
                    if (imageActive === 0 && imageQueue.length === 0) scheduleImageCacheStatsRefresh();
                });
        }
    }

    function updateImageCacheStatus() {
        const diskSize = formatBytes(imageDiskStats.imageBytes);
        byId('settings-image-cache-status').textContent = `磁盘 ${diskSize}`;
    }

    async function refreshImageCacheStatus() {
        try {
            const stats = await nativeRequest('mediaStationCatalog', 'cache_stats', ['cache_stats', '{}']);
            imageDiskStats = {
                imageBytes: Number(stats.imageBytes) || 0,
                imageCount: Number(stats.imageCount) || 0,
            };
            updateImageCacheStatus();
        } catch (error) {
            console.error(`图片缓存统计失败：${friendlyError(error)}`);
            byId('settings-image-cache-status').textContent = '统计失败';
        }
    }

    function updateFrameInterpolationStatus(status) {
        if (!status) return;
        const label = byId('settings-frame-interpolation-status');
        const component = status.component || {};
        const interpolationEntry = byId('player-interpolation');
        const componentInstalled = component.installed === true;
        const componentReady = componentInstalled && status.componentStatus === 'ready';
        interpolationEntry.dataset.componentInstalled = String(componentInstalled);
        interpolationEntry.dataset.componentReady = String(componentReady);
        interpolationEntry.classList.toggle('hidden', !componentInstalled);
        if (typeof videoComponentStatuses !== 'undefined') refreshVideoEnhancementEntry();
        if (component.restarting) interpolationComponentRestarting = true;
        const blocked = interpolationComponentRestarting || component.supported === false || component.busy || component.restartRequired || typeof component.installed !== 'boolean';
        byId('settings-install-frame-interpolation').disabled = Boolean(blocked || component.installed);
        byId('settings-import-frame-interpolation').disabled = Boolean(blocked || component.installed);
        byId('settings-uninstall-frame-interpolation').disabled = Boolean(blocked || !component.installed);
        byId('settings-confirm-uninstall-frame-interpolation').disabled = Boolean(blocked || !component.installed);
        const pendingInstall = component.pendingOperation === 'install';
        byId('settings-pending-frame-interpolation-install').classList.toggle('hidden', !pendingInstall);
        byId('settings-cancel-install-frame-interpolation').disabled = Boolean(interpolationComponentRestarting || component.supported === false || component.busy || !pendingInstall);
        const startupFailure = ['frame_interpolation_component_busy', 'frame_interpolation_component_invalid'].includes(status.failureCode);
        const canRestart = pendingInstall && component.restartRequired && !component.busy && !component.error && component.supported !== false && status.failureCode !== 'frame_interpolation_component_invalid';
        byId('settings-restart-frame-interpolation').disabled = !canRestart || interpolationComponentRestarting;
        byId('settings-confirm-restart-frame-interpolation').disabled = !canRestart || interpolationComponentRestarting;
        byId('settings-later-restart-frame-interpolation').disabled = interpolationComponentRestarting;
        if (!pendingInstall) interpolationRestartPrompted = false;
        if (!canRestart) byId('settings-confirm-frame-interpolation-restart').classList.add('hidden');
        if (canRestart && !startupFailure && !interpolationRestartPrompted && !interpolationComponentRestarting) {
            showFrameInterpolationRestartConfirmation();
        }
        window.clearTimeout(interpolationComponentTimer);
        interpolationComponentTimer = 0;
        interpolationComponentTracking = Boolean(component.busy);
        if (interpolationModels[status.selectedModel]) {
            preferredInterpolationModel = status.selectedModel;
        }
        if (interpolationComponentRestarting) {
            label.textContent = '正在退出并重新启动播放器...';
            label.dataset.state = 'pending';
        } else if (component.supported === false) {
            label.textContent = '插帧组件只支持 Windows x64。';
            label.dataset.state = 'error';
        } else if (component.busy) {
            const progress = component.progress || {};
            label.textContent = progress.phase === 'selecting' ? '请选择本地组件包...' : progress.phase === 'downloading'
                ? `正在下载：${formatBytes(progress.downloadedBytes)} / ${formatBytes(progress.totalBytes)} · ${progress.source}`
                : progress.phase === 'verifying' ? '正在校验并准备安装组件...' : '正在查询组件发行包...';
            label.dataset.state = 'pending';
            scheduleFrameInterpolationStatusRefresh();
        } else if (component.restartRequired) {
            label.textContent = component.pendingOperation === 'uninstall'
                ? '已排队卸载：退出所有播放器实例并重新启动后生效。'
                : '已校验安装包：退出所有播放器实例并重新启动后生效。';
            if (['frame_interpolation_component_busy', 'frame_interpolation_component_invalid'].includes(status.failureCode)) {
                label.textContent = `启动时未能执行组件变更：${status.failureDetail}。待重启操作已保留。`;
            }
            label.dataset.state = 'pending';
            byId('settings-confirm-frame-interpolation-uninstall').classList.add('hidden');
        } else if (component.error) {
            label.textContent = component.error.message;
            label.dataset.state = 'error';
        } else if (component.installed === false) {
            label.textContent = '未安装插帧组件。普通播放不受影响，可按需下载安装。';
            label.dataset.state = 'pending';
        } else if (status.componentStatus === 'ready') {
            const engines = Number.isFinite(status.engineCount) ? `${status.engineCount} 个 Engine` : null;
            const modelCount = Array.isArray(status.models) ? `${status.models.length} 个模型` : null;
            const components = [status.gpuName, modelCount, status.runtimeVersion, engines].filter(Boolean);
            label.textContent = components.join(' · ') || 'RIFE 插帧组件已就绪';
            label.dataset.state = 'ready';
        } else {
            const error = new Error(status.failureDetail || 'RIFE 插帧组件不可用');
            error.code = status.failureCode || 'frame_interpolation_runtime_unavailable';
            label.textContent = friendlyError(error);
            label.dataset.state = 'error';
        }
    }

    function scheduleFrameInterpolationStatusRefresh(delayMs = 1500) {
        window.clearTimeout(interpolationComponentTimer);
        interpolationComponentTimer = window.setTimeout(() => {
            interpolationComponentTimer = 0;
            return refreshFrameInterpolationStatus();
        }, delayMs);
    }

    function showFrameInterpolationRestartConfirmation() {
        if (byId('settings-restart-frame-interpolation').disabled) return;
        interpolationRestartPrompted = true;
        byId('settings-confirm-frame-interpolation-restart').classList.remove('hidden');
        byId('settings-confirm-restart-frame-interpolation').focus();
    }

    function deferFrameInterpolationRestart() {
        if (interpolationComponentRestarting) return;
        byId('settings-confirm-frame-interpolation-restart').classList.add('hidden');
        byId('settings-restart-frame-interpolation').focus();
    }

    async function restartForFrameInterpolationInstall() {
        if (interpolationComponentRestarting || byId('settings-confirm-restart-frame-interpolation').disabled) return;
        interpolationComponentRestarting = true;
        for (const id of ['settings-restart-frame-interpolation', 'settings-confirm-restart-frame-interpolation', 'settings-later-restart-frame-interpolation', 'settings-cancel-install-frame-interpolation']) {
            byId(id).disabled = true;
        }
        const label = byId('settings-frame-interpolation-status');
        label.textContent = '正在准备退出并重新启动播放器...';
        label.dataset.state = 'pending';
        try {
            const result = await nativeRequest('mediaStationFrameInterpolation', 'frame_interpolation_component_restart', ['frame_interpolation_component_restart'], 15000);
            if (result?.restarting !== true) throw new Error('重启助手未确认退出与重新启动请求');
            label.textContent = '正在退出并重新启动播放器...';
        } catch (error) {
            interpolationComponentRestarting = false;
            await refreshFrameInterpolationStatus();
            label.textContent = `自动重启请求失败：${friendlyError(error)}。待安装操作仍保留；不会自动重复重启。`;
            label.dataset.state = 'error';
        }
    }

    async function changeFrameInterpolationComponent(operation) {
        interpolationComponentTracking = true;
        byId('settings-install-frame-interpolation').disabled = true;
        byId('settings-uninstall-frame-interpolation').disabled = true;
        byId('settings-confirm-uninstall-frame-interpolation').disabled = true;
        byId('settings-cancel-install-frame-interpolation').disabled = true;
        try {
            const status = await nativeRequest('mediaStationFrameInterpolation', operation, [operation], 15000);
            updateFrameInterpolationStatus(status);
        } catch (error) {
            await refreshFrameInterpolationStatus();
            const label = byId('settings-frame-interpolation-status');
            label.textContent = `组件操作失败：${friendlyError(error)}`;
            label.dataset.state = 'error';
        }
    }

    async function refreshFrameInterpolationStatus() {
        try {
            const status = await nativeRequest(
                'mediaStationFrameInterpolation',
                'frame_interpolation_status',
                ['frame_interpolation_status'],
                15000,
            );
            updateFrameInterpolationStatus(status);
        } catch (error) {
            const label = byId('settings-frame-interpolation-status');
            label.textContent = friendlyError(error);
            label.dataset.state = 'error';
            if (interpolationComponentTracking) {
                label.textContent += '；正在重新查询后台组件任务状态。';
                scheduleFrameInterpolationStatusRefresh(3000);
            }
        }
    }

    function scheduleImageCacheStatsRefresh() {
        window.clearTimeout(imageStatsTimer);
        imageStatsTimer = window.setTimeout(() => {
            imageStatsTimer = 0;
            refreshImageCacheStatus();
        }, 500);
    }

    function formatBytes(bytes) {
        const value = Math.max(0, Number(bytes) || 0);
        if (value < 1024) return `${value} B`;
        if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
        if (value < 1024 * 1024 * 1024) return `${(value / 1024 / 1024).toFixed(1)} MB`;
        return `${(value / 1024 / 1024 / 1024).toFixed(2)} GB`;
    }

    function episodePosition(card) {
        const episode = card.indexNumber ? `第 ${card.indexNumber} 集` : '';
        const season = card.parentIndexNumber ? `第 ${card.parentIndexNumber} 季` : '';
        return [season, episode].filter(Boolean).join(' · ');
    }

    function strokeIcon(name) {
        const paths = {
            film: 'M4 3h16v18H4ZM8 3v18M16 3v18M4 8h4M4 16h4M16 8h4M16 16h4',
            tv: 'M8 3l4 4 4-4M7 21h10M3 8h18v11H3Z',
            grid: 'M4 4h6v6H4ZM14 4h6v6h-6ZM4 14h6v6H4ZM14 14h6v6h-6Z',
            list: 'M9 6h12M9 12h12M9 18h12M3 6h1M3 12h1M3 18h1',
            play: 'm8 5 11 7-11 7Z',
            pause: 'M8 5v14M16 5v14',
            back: 'M8 4 4 8l4 4M4 8h9a8 8 0 1 1-8 10',
            forward: 'm16 4 4 4-4 4M20 8h-9a8 8 0 1 0 8 10',
            subtitles: 'M3 5h18v14H3ZM6 10h4M14 10h4M6 14h12',
            audio: 'M9 17V5l10-2v12M9 5l10-2M9 17a3 3 0 1 1-3-3h3M19 15a3 3 0 1 1-3-3h3',
            speed: 'M4 18a9 9 0 1 1 16 0ZM12 14l5-5',
            more: 'M5 12h.01M12 12h.01M19 12h.01',
            eye: 'M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12ZM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6',
            close: 'm6 6 12 12M6 18 18 6',
            appearance: 'M12 3a9 9 0 1 0 0 18ZM12 3a9 9 0 0 1 0 18V3',
            settings: 'm9 3-1 3-3 1-2 3 2 2v3l3 2 1 4h6l1-4 3-2v-3l2-2-2-3-3-1-1-3ZM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6',
            network: 'M12 3v6M5 16v5M19 16v5M5 16H2V9h20v7h-3ZM5 12h.01M9 12h.01M12 12h7',
            update: 'M20 8V3l-4 4M20 8a9 9 0 1 0 1 8',
            volume: 'M11 5 6 9H3v6h3l5 4ZM15 8a6 6 0 0 1 0 8M18 5a10 10 0 0 1 0 14',
            muted: 'M11 5 6 9H3v6h3l5 4ZM16 9l6 6M16 15l6-6',
            fullscreen: 'M8 3H3v5M16 3h5v5M21 16v5h-5M8 21H3v-5',
            restore: 'M3 8h5V3M16 3v5h5M21 16h-5v5M8 21v-5H3',
            interpolation: 'M8 4h8v16H8ZM4 7H2v10h2M20 7h2v10h-2',
            danmaku: 'M3 4h18v13H9l-5 4v-4H3ZM6 8h12M6 12h8',
            info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM12 11v6M12 7h.01',
        };
        const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
        icon.setAttribute('viewBox', '0 0 24 24'); icon.setAttribute('aria-hidden', 'true');
        icon.classList.add('shell-icon');
        const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
        path.setAttribute('d', paths[name]); icon.append(path);
        return icon;
    }

    function playerIcon(name) {
        const filledPaths = {
            play: 'M8 17.175V6.825q0-.425.3-.713t.7-.287q.125 0 .263.037t.262.113l8.15 5.175q.225.15.338.375t.112.475t-.112.475t-.338.375l-8.15 5.175q-.125.075-.262.113T9 18.175q-.4 0-.7-.288t-.3-.712',
            pause: 'M16 19q-.825 0-1.412-.587T14 17V7q0-.825.588-1.412T16 5t1.413.588T18 7v10q0 .825-.587 1.413T16 19m-8 0q-.825 0-1.412-.587T6 17V7q0-.825.588-1.412T8 5t1.413.588T10 7v10q0 .825-.587 1.413T8 19',
            back: 'M11 16.7V7.3q0-.675-.6-.975t-1.15.1L3.1 11q-.45.325-.45 1t.45 1l6.15 4.575q.55.4 1.15.1t.6-.975m10 0V7.3q0-.675-.6-.975t-1.15.1L13.1 11q-.45.325-.45 1t.45 1l6.15 4.575q.55.4 1.15.1t.6-.975',
            forward: 'M13 16.7V7.3q0-.675.6-.975t1.15.1L20.9 11q.45.325.45 1t-.45 1l-6.15 4.575q-.55.4-1.15.1t-.6-.975m-10 0V7.3q0-.675.6-.975t1.15.1L10.9 11q.45.325.45 1t-.45 1l-6.15 4.575q-.55.4-1.15.1t-.6-.975',
        };
        if (filledPaths[name]) {
            const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
            icon.setAttribute('viewBox', '0 0 24 24');
            icon.setAttribute('aria-hidden', 'true');
            icon.setAttribute('focusable', 'false');
            icon.classList.add('player-tool-icon', 'player-solid-icon');
            if (name === 'back' || name === 'forward') icon.classList.add('player-seek-icon');
            const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
            path.setAttribute('d', filledPaths[name]);
            icon.append(path);
            return icon;
        }
        const icon = strokeIcon(name);
        icon.classList.add('player-tool-icon');
        return icon;
    }

    function cardSubtitle(card, omitSeriesName = false, includeEpisodeTitle = false) {
        if (card.type === 'Episode') {
            return [
                omitSeriesName ? '' : card.seriesName,
                episodePosition(card),
                includeEpisodeTitle ? card.title : '',
            ].filter(Boolean).join(' · ');
        }
        return [card.year, ...(card.genres || []).slice(0, 2)].filter(Boolean).join(' · ');
    }

    function cardTitle(card, preferSeriesName = false) {
        return preferSeriesName && card.type === 'Episode' && card.seriesName ? card.seriesName : card.title;
    }

    function progressPercent(card) {
        if (!card.durationMs || !card.resumePositionMs) return 0;
        return Math.max(0, Math.min(100, card.resumePositionMs / card.durationMs * 100));
    }

    // 对齐标准 Emby NextUp 语义：优先“最近在看”的单集（按 LastPlayedDate），
    // 没有在看中的就给第一个未看完/未看的，最后才回落到第 1 集。
    function playedAtTimestamp(episode) {
        const raw = episode?.lastPlayedAt;
        if (!raw) return 0;
        const parsed = Date.parse(raw);
        return Number.isFinite(parsed) ? parsed : 0;
    }

    function pickContinueEpisode(episodes) {
        if (!Array.isArray(episodes) || episodes.length === 0) return null;
        const inProgress = episodes.filter((episode) => episode.resumePositionMs > 0 && !episode.played);
        if (inProgress.length > 0) {
            return inProgress.reduce((latest, episode) => (
                playedAtTimestamp(episode) > playedAtTimestamp(latest) ? episode : latest
            ));
        }
        return episodes.find((episode) => !episode.played) ?? null;
    }

    function createMediaCard(card, options = {}) {
        const title = cardTitle(card, options.episodeAsSeries);
        const isLibrary = options.library || (options.landscape && card.type === 'CollectionFolder');
        const rowKey = String(options.key || '');
        const cardVariant = isLibrary
            ? ' library-card'
            : rowKey === 'resume'
                ? ' resume-card'
                : rowKey.startsWith('latest')
                    ? ' latest-card'
                    : '';
        const button = element('button', `media-card${options.landscape ? ' landscape-card' : ''}${cardVariant}`);
        button.type = 'button';
        button.dataset.cardIndex = String(options.index ?? 0);
        button.dataset.mediaId = card.id;
        button.dataset.focusKey = `media:${card.id}`;
        const art = element('span', 'card-art');
        const fallback = element('span', 'art-fallback', initials(title));
        const img = document.createElement('img');
        img.alt = '';
        img.decoding = 'async';
        art.append(fallback, img);
        const ref = rowKey === 'resume' && card.type === 'Episode'
            ? (card.primaryImage || card.landscapeImage)
            : options.landscape
                ? (card.landscapeImage || card.primaryImage)
                : (card.primaryImage || card.landscapeImage);
        observeImage(img, ref, imageWidthFor(ref, options.landscape));
        const progress = progressPercent(card);
        if (progress > 0) {
            const track = element('span', 'card-progress');
            const value = element('span');
            value.style.width = `${progress}%`;
            track.append(value);
            art.append(track);
        }
        if (rowKey === 'resume' && card.durationMs > card.resumePositionMs) {
            art.append(element('span', 'card-remaining', `还剩 ${Math.floor((card.durationMs - card.resumePositionMs) / 60000)}分钟`));
        }
        if (isLibrary) {
            button.setAttribute('aria-label', '打开媒体库：' + title);
            button.title = title; fallback.textContent = title; button.append(art);
        } else {
            const resumeEpisodeTitle = options.resumePage && card.type === 'Episode'
                && card.title !== title && !episodePosition(card).includes(card.title) ? card.title : '';
            const subtitle = rowKey === 'resume'
                ? [card.type === 'Episode' ? episodePosition(card) : card.year, resumeEpisodeTitle, `已看到 ${formatTime(card.resumePositionMs || 0)}`].filter(Boolean).join(' · ')
                : options.episodePicker
                ? formatDuration(card.durationMs)
                : cardSubtitle(
                    card,
                    options.omitSeriesName ?? options.episodeAsSeries,
                    rowKey === 'resume',
                );
            const caption = options.episodePicker && card.indexNumber
                ? `第 ${card.indexNumber} 集 · ${title}`
                : rowKey === 'resume' && card.type === 'Episode' && !options.resumePage
                    ? [title, card.title].filter(Boolean).join(' · ') : title;
            button.title = [caption, subtitle].filter(Boolean).join('\n');
            button.append(art, element('span', 'card-title', caption), element('span', 'card-subtitle', subtitle));
        }
        button.addEventListener('focus', () => options.onFocus?.(card));
        button.addEventListener('click', () => options.onClick?.(card));
        return button;
    }

    function createRowCarousel(row, title, controlsHost = null, options = {}) {
        row.classList.add('carousel-row');
        const rowShell = element('div', 'media-row-carousel');
        if (row.classList.contains('home-media-row')) rowShell.classList.add('home-row-carousel');
        if (row.classList.contains('episode-row')) rowShell.classList.add('episode-row-carousel');
        const previous = element('button', 'icon-button hero-carousel-button hero-carousel-previous media-row-carousel-button media-row-carousel-previous');
        previous.type = 'button';
        previous.title = '向左滚动';
        previous.setAttribute('aria-label', `${title}向左滚动`);
        previous.append(element('span', 'hero-carousel-chevron'));
        const next = element('button', 'icon-button hero-carousel-button hero-carousel-next media-row-carousel-button media-row-carousel-next');
        next.type = 'button';
        next.title = '向右滚动';
        next.setAttribute('aria-label', `${title}向右滚动`);
        next.append(element('span', 'hero-carousel-chevron'));
        const controls = element('div', 'row-carousel-controls');
        controls.setAttribute('role', 'group');
        controls.setAttribute('aria-label', `${title}分页`);
        controls.append(previous, next);

        let pageDistance = 0;
        let cardStride = 0;
        let pageIndex = 0;
        let maximumPage = 0;
        let active = false;
        let pendingDirection = 0;
        let generation = 0;
        let initialScrollTarget = options.initialScrollTarget || null;
        let carouselReady = false;
        let pendingSmoothRequest = null;
        const refreshControls = () => {
            const atStart = maximumPage < 1 || pageIndex <= 0;
            const atEnd = maximumPage < 1 || pageIndex >= maximumPage;
            previous.disabled = atStart;
            next.disabled = atEnd;
            previous.classList.toggle('unavailable', atStart);
            next.classList.toggle('unavailable', atEnd);
        };
        const measurePageDistance = () => {
            const card = row.querySelector('.media-card');
            if (!card) return row.clientWidth;
            row.style.removeProperty('--row-card-width');
            row.style.removeProperty('--row-gap');
            const rawGap = Number.parseFloat(window.getComputedStyle(row).columnGap) || 0;
            const rawWidth = card.getBoundingClientRect().width;
            const pixelRatio = window.devicePixelRatio || 1;
            const gap = Math.round(rawGap * pixelRatio) / pixelRatio;
            const preferredCardWidth = Math.round(rawWidth * pixelRatio) / pixelRatio;
            let cardWidth = preferredCardWidth;
            if (row.classList.contains('people-row')) {
                const cardCount = row.querySelectorAll(':scope > .media-card').length;
                const preferredContentWidth = cardCount * preferredCardWidth + Math.max(0, cardCount - 1) * gap;
                if (cardCount > 1 && preferredContentWidth > row.clientWidth + 0.5) {
                    const wholeCardCount = clamp(
                        Math.round((row.clientWidth + gap) / (preferredCardWidth + gap)),
                        1,
                        cardCount,
                    );
                    const fittedWidth = (row.clientWidth - gap * (wholeCardCount - 1)) / wholeCardCount;
                    cardWidth = Math.max(1 / pixelRatio, Math.floor(fittedWidth * pixelRatio) / pixelRatio);
                }
            }
            row.style.setProperty('--row-card-width', `${cardWidth}px`);
            row.style.setProperty('--row-gap', `${gap}px`);
            cardStride = cardWidth + gap;
            return Math.max(
                cardStride,
                Math.floor((row.clientWidth * 0.78) / cardStride) * cardStride,
            );
        };
        const alignEndToPage = () => {
            // Player mode hides the app shell with display:none. Native window
            // changes can still emit resize events while it is hidden, so do
            // not replace the last valid card metrics with zero-width values.
            if (!row.isConnected || row.clientWidth <= 0 || !row.getClientRects().length) return;
            const previousDistance = pageDistance;
            const previousIndex = previousDistance > 0
                ? Math.round(row.scrollLeft / previousDistance)
                : 0;
            generation += 1;
            active = false;
            pendingDirection = 0;
            stopSmoothScroll(row);
            row.style.setProperty('--row-end-alignment', '0px');
            pageDistance = measurePageDistance();
            if (pageDistance <= 0) {
                maximumPage = 0;
                pageIndex = 0;
                refreshControls();
                return;
            }
            const limit = scrollLimit(row, 'x');
            maximumPage = Math.ceil(Math.max(0, limit - 0.5) / pageDistance);
            const alignment = Math.max(0, maximumPage * pageDistance - limit);
            row.style.setProperty('--row-end-alignment', `${alignment}px`);
            const art = row.querySelector('.card-art');
            if (art) rowShell.style.setProperty('--row-button-center', `${art.getBoundingClientRect().height / 2}px`);
            const requestedInitialPosition = initialScrollTarget?.();
            initialScrollTarget = null;
            if (Number.isFinite(requestedInitialPosition)) {
                row.scrollLeft = clamp(requestedInitialPosition, 0, limit);
                pageIndex = clamp(Math.round(row.scrollLeft / pageDistance), 0, maximumPage);
            } else {
                pageIndex = clamp(previousIndex, 0, maximumPage);
                row.scrollLeft = pageIndex * pageDistance;
            }
            refreshControls();
            carouselReady = true;
            const pending = pendingSmoothRequest;
            pendingSmoothRequest = null;
            if (pending) startSmoothCarouselTo(pending.getPosition()).then(pending.resolve);
        };
        let refreshFrame = 0;
        row.addEventListener('scroll', () => {
            if (refreshFrame) return;
            refreshFrame = window.requestAnimationFrame(() => {
                refreshFrame = 0;
                if (!active && pageDistance > 0) {
                    pageIndex = clamp(Math.round(row.scrollLeft / pageDistance), 0, maximumPage);
                }
                refreshControls();
            });
        }, { passive: true });
        const moveToPage = (requestedPage) => {
            if (active) {
                pendingDirection = requestedPage > pageIndex ? 1 : -1;
                return;
            }
            const targetPage = clamp(requestedPage, 0, maximumPage);
            if (targetPage === pageIndex || pageDistance <= 0) {
                refreshControls();
                return;
            }
            const token = ++generation;
            active = true;
            pageIndex = targetPage;
            refreshControls();
            smoothScrollTo(
                row,
                { left: targetPage * pageDistance },
                horizontalScrollDurationMs,
            ).then((completed) => {
                if (token !== generation) return;
                if (completed) {
                    row.scrollLeft = targetPage * pageDistance;
                } else {
                    pageIndex = clamp(Math.round(row.scrollLeft / pageDistance), 0, maximumPage);
                    row.scrollLeft = pageIndex * pageDistance;
                }
                active = false;
                refreshControls();
                const queuedDirection = pendingDirection;
                pendingDirection = 0;
                if (queuedDirection) moveToPage(pageIndex + queuedDirection);
            });
        };
        previous.addEventListener('click', () => moveToPage(pageIndex - 1));
        next.addEventListener('click', () => moveToPage(pageIndex + 1));
        row._refreshCarousel = alignEndToPage;
        row._jumpCarouselTo = (requestedPosition) => {
            generation += 1;
            active = false;
            pendingDirection = 0;
            stopSmoothScroll(row);
            row.scrollLeft = clamp(requestedPosition, 0, scrollLimit(row, 'x'));
            pageIndex = pageDistance > 0
                ? clamp(Math.round(row.scrollLeft / pageDistance), 0, maximumPage)
                : 0;
            refreshControls();
        };
        const startSmoothCarouselTo = (requestedPosition) => {
            const token = ++generation;
            active = true;
            pendingDirection = 0;
            const target = clamp(requestedPosition, 0, scrollLimit(row, 'x'));
            pageIndex = pageDistance > 0
                ? clamp(Math.round(target / pageDistance), 0, maximumPage)
                : 0;
            refreshControls();
            return smoothScrollTo(row, { left: target }, horizontalScrollDurationMs).then((completed) => {
                if (token !== generation) return false;
                active = false;
                if (completed) {
                    pageIndex = pageDistance > 0
                        ? clamp(Math.round(row.scrollLeft / pageDistance), 0, maximumPage)
                        : 0;
                }
                refreshControls();
                return completed;
            });
        };
        row._smoothCarouselTo = (requestedPosition) => {
            const getPosition = typeof requestedPosition === 'function'
                ? requestedPosition
                : () => requestedPosition;
            if (carouselReady) return startSmoothCarouselTo(getPosition());
            return new Promise((resolve) => {
                if (pendingSmoothRequest) pendingSmoothRequest.resolve(false);
                pendingSmoothRequest = { getPosition, resolve };
            });
        };
        row._revealCarouselNode = (node) => {
            const cards = [...row.querySelectorAll('.media-card')];
            const cardIndex = cards.indexOf(node.closest('.media-card'));
            if (cardIndex < 0 || cardStride <= 0) return;
            const cardsPerPage = Math.max(1, Math.round(pageDistance / cardStride));
            moveToPage(Math.floor(cardIndex / cardsPerPage));
        };
        rowShell.append(row);
        (controlsHost || rowShell).append(controls);
        window.requestAnimationFrame(alignEndToPage);
        return rowShell;
    }

    function refreshMediaRowCarousels() {
        content.querySelectorAll('.carousel-row').forEach((row) => row._refreshCarousel?.());
    }

    function createSection(title, cards, options = {}) {
        if (!cards?.length) return null;
        const section = element('section', 'media-section');
        const heading = element('div', 'section-heading');
        heading.append(element('h2', '', title));
        if (options.description) heading.firstElementChild.append(element('small', 'section-description', options.description));
        const headingActions = element('div', 'section-heading-actions');
        if (options.more) {
            const more = element('button', '', '查看全部');
            more.type = 'button';
            more.addEventListener('click', options.more);
            headingActions.append(more);
        }
        const row = element('div', `media-row home-media-row${options.landscape ? ' landscape' : ''}`);
        row.dataset.rowKey = options.key || title;
        row.dataset.rowIndex = String(options.rowIndex ?? 0);
        cards.forEach((card, index) => row.append(createMediaCard(card, { ...options, index })));
        const carousel = createRowCarousel(row, title, headingActions);
        heading.append(headingActions);
        section.append(heading, carousel);
        return section;
    }

    function resumePlayback(card) {
        return startPlayback(card, card.resumePositionMs || 0);
    }

    function captureMediaRowRects(rowKey) {
        const row = content.querySelector(`.media-row[data-row-key="${rowKey}"]`);
        if (!row) return new Map();
        return new Map([...row.querySelectorAll('.media-card[data-media-id]')]
            .map((card) => [card.dataset.mediaId, card.getBoundingClientRect()]));
    }

    function animateMediaRowReorder(rowKey, previousRects, emphasizedId) {
        if (prefersReducedMotion()) return;
        requestAnimationFrame(() => requestAnimationFrame(() => {
            const row = content.querySelector(`.media-row[data-row-key="${rowKey}"]`);
            if (!row) return;
            for (const card of row.querySelectorAll('.media-card[data-media-id]')) {
                const id = card.dataset.mediaId;
                const before = previousRects.get(id);
                const after = card.getBoundingClientRect();
                let keyframes;
                if (before?.width > 0 && before?.height > 0) {
                    const deltaX = before.left - after.left;
                    const deltaY = before.top - after.top;
                    if (Math.abs(deltaX) < 1 && Math.abs(deltaY) < 1) continue;
                    keyframes = [
                        { transform: `translate3d(${deltaX}px, ${deltaY}px, 0)` },
                        { transform: 'translate3d(0, 0, 0)' },
                    ];
                } else if (id === emphasizedId) {
                    keyframes = [
                        { opacity: 0.35, transform: 'translate3d(0, 12px, 0) scale(0.98)' },
                        { opacity: 1, transform: 'translate3d(0, 0, 0) scale(1)' },
                    ];
                } else {
                    continue;
                }
                card.style.zIndex = id === emphasizedId ? '2' : '1';
                card.style.willChange = 'transform, opacity';
                const animation = card.animate(keyframes, {
                    duration: id === emphasizedId ? 440 : 360,
                    easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
                });
                const cleanup = () => {
                    card.style.removeProperty('z-index');
                    card.style.removeProperty('will-change');
                };
                animation.finished.then(cleanup, cleanup);
            }
        }));
    }

    function heroArtworkFits(hero, art) {
        const width = hero.clientWidth;
        const stacked = content.clientWidth <= 740;
        const height = stacked ? Math.min(width * 9 / 16, 300) : hero.clientHeight;
        const availableWidth = stacked ? width : width * .7;
        const scale = (stacked ? Math.max : Math.min)(availableWidth / art.width, height / art.height);
        const density = window.devicePixelRatio || 1;
        return art.width >= 960 && art.width / art.height >= 1.45
            && art.width >= Math.ceil(art.width * scale * density)
            && art.height >= Math.ceil(art.height * scale * density);
    }

    async function loadHeroArtwork(hero, card) {
        const expectedSession = session;
        const ref = card.backdropImage || card.landscapeImage;
        const requestWidth = imageWidthFor(ref, true, true);
        const key = JSON.stringify([session?.baseUrl, session?.userId, ref, requestWidth]);
        let art = heroArtworkCache.get(key);
        if (!art) {
            const src = await requestImage(ref, requestWidth, { background: true });
            const image = new Image(); image.src = src; await image.decode();
            if (session !== expectedSession) throw new Error('账号已切换，取消旧账号精选图片');
            art = { src, width: image.naturalWidth, height: image.naturalHeight, requestWidth };
            heroArtworkCache.set(key, art);
        }
        return art;
    }

    function refreshHeroArtworkQualification() {
        if (currentView?.kind !== 'home') return;
        const hero = content.querySelector('.home-hero');
        if (!hero?._artworkPool) return;
        const cards = hero._artworkPool.filter((entry) => heroArtworkFits(hero, entry.art)).slice(0, heroCarouselMaxCards);
        const requestWidth = imageWidthFor(null, true, true);
        if (!hero._refreshingArtwork && hero._artworkPool.some((entry) => entry.art.requestWidth < requestWidth && !heroArtworkFits(hero, entry.art))) {
            hero._refreshingArtwork = true;
            const revision = heroRevision;
            Promise.allSettled(hero._artworkPool.map(async (entry) => ({ card: entry.card, art: await loadHeroArtwork(hero, entry.card) }))).then((results) => {
                hero._refreshingArtwork = false;
                if (revision !== heroRevision || !hero.isConnected) return;
                results.filter((result) => result.status === 'rejected').forEach((result) => console.error('精选高清图片重新加载失败：' + friendlyError(result.reason)));
                hero._artworkPool = results.filter((result) => result.status === 'fulfilled').map((result) => result.value);
                refreshHeroArtworkQualification();
            });
        }
        const signature = cards.map((entry) => entry.card.id).join(',');
        if (signature === hero.dataset.qualifiedIds) return;
        const selectedId = hero.dataset.mediaId;
        heroCarouselController?.destroy(); heroCarouselController = null;
        hero.dataset.qualifiedIds = signature; hero.dataset.mediaId = '';
        hero.replaceChildren(element('div', 'hero-slide'), element('div', 'hero-slide'));
        hero.classList.toggle('hero-pending', !cards.length);
        if (cards.length) startHeroCarousel(hero, cards, selectedId);
    }

    async function updateHero(hero, card, art, direction = 1) {
        if (!hero.isConnected || hero.dataset.mediaId === card.id) return;
        const copy = element('div', 'hero-copy');
        copy.append(element('p', 'hero-kicker', '最近添加'));
        copy.append(element('h1', '', cardTitle(card, true)));
        const meta = element('p', 'hero-meta');
        [card.year, ...(card.genres || []).slice(0, 2), card.dynamicRange]
            .filter(Boolean).forEach((value) => meta.append(element('span', '', value)));
        copy.append(meta);
        if (card.overview) copy.append(element('p', 'hero-overview', card.overview.length > 80 ? card.overview.slice(0, 80).trimEnd() + '…' : card.overview));
        const actions = element('div', 'hero-actions');
        const primary = element('button', 'primary-command hero-primary'); primary.type = 'button';
        primary.dataset.focusKey = `hero:primary:${card.id}`;
        primary.append(strokeIcon('play'), element('span', '', '打开作品'));
        primary.addEventListener('click', () => openDetail(card)); actions.append(primary, createFavoriteButton(card)); copy.append(actions);
        const image = document.createElement('img');
        image.className = 'hero-image'; image.alt = ''; image.src = art.src;
        const slides = [...hero.querySelectorAll('.hero-slide')];
        const active = slides.find((slide) => slide.classList.contains('active'));
        const next = slides.find((slide) => slide !== active);
        const focusInside = active?.contains(document.activeElement);
        slides.forEach((slide) => { slide.classList.remove('sliding'); slide.inert = true; slide.setAttribute('aria-hidden', 'true'); });
        next.replaceChildren(image, copy);
        next.style.transform = `translateX(${direction * 100}%)`;
        const animate = Boolean(active) && !prefersReducedMotion();
        void next.offsetWidth;
        if (animate) { next.classList.add('sliding'); active.classList.add('sliding'); }
        next.classList.add('active'); next.inert = false; next.setAttribute('aria-hidden', 'false');
        next.style.transform = 'translateX(0)';
        if (active) { active.classList.remove('active'); active.style.transform = `translateX(${-direction * 100}%)`; }
        hero.dataset.mediaId = card.id;
        if (focusInside) primary.focus({ preventScroll: true });
        if (animate) await new Promise((resolve) => window.setTimeout(resolve, 320));
        slides.forEach((slide) => { slide.classList.remove('sliding'); if (slide !== next) { slide.replaceChildren(); slide.style.transform = 'translateX(100%)'; } });
    }

    function stopHeroCarousel() {
        window.clearTimeout(heroRotationTimer); heroRotationTimer = 0;
        heroRevision += 1;
        heroCarouselController?.destroy(); heroCarouselController = null;
    }

    function heroCandidates(data, latestSections) {
        const seen = new Set();
        return [...latestSections.flatMap((section) => section.items || []), ...(data.latest || []), ...(data.resume || [])]
            .filter((card) => {
                const key = JSON.stringify(card?.backdropImage || card?.landscapeImage);
                if (!card?.id || card.type === 'Episode' || card.type === 'CollectionFolder' || key === undefined || key === 'null' || seen.has(key)) return false;
                seen.add(key); return true;
            }).slice(0, 20);
    }

    function startHeroCarousel(hero, cards, selectedId = '') {
        let index = -1; let destroyed = false; let busy = false;
        const pauseReasons = new Set();
        const controls = element('div', 'hero-carousel-controls');
        const dots = element('div', 'hero-carousel-dots'); dots.setAttribute('role', 'group'); dots.setAttribute('aria-label', '精选作品');
        const schedule = () => {
            window.clearTimeout(heroRotationTimer);
            if (destroyed || busy || pauseReasons.size || cards.length < 2) return;
            heroRotationTimer = window.setTimeout(() => void select((index + 1) % cards.length, 1), heroRotationIntervalMs);
        };
        const select = async (nextIndex, direction) => {
            if (destroyed || busy || nextIndex === index) return;
            const focusedControl = controls.contains(document.activeElement) ? document.activeElement : null;
            busy = true; window.clearTimeout(heroRotationTimer);
            controls.querySelectorAll('button').forEach((button) => { button.disabled = true; });
            index = nextIndex;
            [...dots.children].forEach((dot, n) => dot.setAttribute('aria-pressed', String(n === index)));
            await updateHero(hero, cards[index].card, cards[index].art, direction);
            busy = false;
            if (destroyed) return;
            controls.querySelectorAll('button').forEach((button) => { button.disabled = false; });
            if (focusedControl?.isConnected) focusedControl.focus({ preventScroll: true });
            schedule();
        };
        cards.forEach((entry, n) => {
            const dot = element('button', 'hero-dot'); dot.type = 'button';
            dot.setAttribute('aria-label', `轮播作品 ${n + 1}：${entry.card.title}`); dot.setAttribute('aria-pressed', 'false');
            dot.addEventListener('click', () => void select(n, n < index ? -1 : 1)); dots.append(dot);
        });
        controls.append(dots);
        for (const [offset, label] of [[-1, '上一部精选'], [1, '下一部精选']]) {
            const button = element('button', 'icon-button hero-carousel-button ' + (offset < 0 ? 'hero-carousel-previous' : 'hero-carousel-next'));
            button.type = 'button'; button.setAttribute('aria-label', label); button.title = label;
            button.append(element('span', 'hero-carousel-chevron'));
            button.addEventListener('click', () => void select((index + offset + cards.length) % cards.length, offset)); controls.append(button);
        }
        if (cards.length > 1) hero.append(controls);
        const pause = (reason) => { pauseReasons.add(reason); window.clearTimeout(heroRotationTimer); };
        const resume = (reason) => { pauseReasons.delete(reason); schedule(); };
        const enter = () => pause('pointer'); const leave = () => resume('pointer');
        const focus = () => pause('focus'); const blur = (event) => { if (!hero.contains(event.relatedTarget)) resume('focus'); };
        const visibility = () => document.hidden ? pause('hidden') : resume('hidden');
        hero.addEventListener('pointerenter', enter); hero.addEventListener('pointerleave', leave);
        hero.addEventListener('focusin', focus); hero.addEventListener('focusout', blur);
        document.addEventListener('visibilitychange', visibility);
        if (document.hidden) pause('hidden'); if (player) pause('player');
        heroCarouselController = { pause, resume, destroy() {
            destroyed = true; window.clearTimeout(heroRotationTimer);
            hero.removeEventListener('pointerenter', enter); hero.removeEventListener('pointerleave', leave);
            hero.removeEventListener('focusin', focus); hero.removeEventListener('focusout', blur);
            document.removeEventListener('visibilitychange', visibility);
        } };
        const restored = cards.findIndex((entry) => entry.card.id === selectedId || currentView?.focusKey?.endsWith(':' + entry.card.id));
        void select(restored >= 0 ? restored : 0, 1);
    }

    async function loadHeroCandidates(hero, candidates, revision) {
        hero._artworkPool = [];
        for (const card of candidates) {
            if (revision !== heroRevision || !hero.isConnected) return;
            try {
                const art = await loadHeroArtwork(hero, card);
                if (revision !== heroRevision || !hero.isConnected) return;
                hero._artworkPool.push({ card, art });
                // Reveal the first sharp image immediately; complete the carousel in the background.
                if (!hero.dataset.qualifiedIds && heroArtworkFits(hero, art)) refreshHeroArtworkQualification();
                if (hero._artworkPool.filter((entry) => heroArtworkFits(hero, entry.art)).length >= heroCarouselMaxCards) break;
            } catch (error) { console.error('精选图片加载失败：' + friendlyError(error)); }
        }
        if (revision === heroRevision && hero.isConnected) refreshHeroArtworkQualification();
    }

    function renderHome(data) {
        stopHeroCarousel(); replaceContent();
        const revision = heroRevision;
        const resumeItems = Array.isArray(data.resume) ? data.resume.slice(0, maximumContinueWatchingItems) : [];
        const latestSections = Array.isArray(data.latestSections) ? data.latestSections.filter((section) => section?.library && section.items?.length) : [];
        const hero = element('section', 'home-hero hero-pending');
        hero.setAttribute('aria-label', '最近添加精选');
        hero.append(element('div', 'hero-slide'), element('div', 'hero-slide'));
        const band = element('div', 'page-band');
        const resume = createSection('继续观看', resumeItems, {
            key: 'resume', rowIndex: 1, landscape: true, episodeAsSeries: true, description: '接着上次的故事',
            more: () => setCurrentView({ kind: 'resume', data: homeData }),
            onClick: resumePlayback,
        });
        const latestRows = latestSections.length
            ? latestSections.map((section, index) => createSection(`最近添加 · ${section.library.title}`, section.items, {
                key: `latest:${section.library.id}`, rowIndex: index + 2, onClick: openDetail,
                more: () => openLibrary(section.library),
            }))
            : [createSection('最近添加', data.latest, { key: 'latest', rowIndex: 2, onClick: openDetail })];
        [resume, ...latestRows].filter(Boolean).forEach((section) => band.append(section));
        if (!band.childElementCount) band.append(element('div', 'empty-state', '媒体库暂无内容'));
        content.append(hero, band);
        const candidates = heroCandidates(data, latestSections);
        void loadHeroCandidates(hero, candidates, revision);
    }

    function renderResume(data) {
        replaceContent();
        const header = element('div', 'page-header'); header.append(element('h1', '', '继续观看'));
        const grid = element('div', 'grid-view resume-grid');
        (data.resume || []).forEach((card, index) => grid.append(createMediaCard(card, {
            key: 'resume', index, landscape: true, episodeAsSeries: true, resumePage: true,
            onClick: resumePlayback,
        })));
        content.append(header, grid.childElementCount ? grid : element('div', 'empty-state', '暂无继续观看的作品'));
    }

    function createFavoriteButton(card) {
        const button = element('button', 'secondary-command favorite-command'); button.type = 'button';
        button.dataset.favoriteId = card.id; button.dataset.focusKey = `favorite:${card.id}`;
        const refresh = () => {
            button.textContent = card.isFavorite === true ? '✓ 已收藏' : card.isFavorite === false ? '+ 收藏' : '收藏';
            if (typeof card.isFavorite === 'boolean') button.setAttribute('aria-pressed', String(card.isFavorite));
            else { button.removeAttribute('aria-pressed'); button.title = '先读取服务器收藏状态'; }
        };
        refresh();
        button.addEventListener('click', async () => {
            button.disabled = true; button.setAttribute('aria-busy', 'true');
            try {
                if (typeof card.isFavorite !== 'boolean') {
                    const detail = await nativeRequest('mediaStationCatalog', 'detail', ['detail', JSON.stringify({ mediaId: card.id, refresh: true })]);
                    if (detail.item?.id !== card.id || typeof detail.item.isFavorite !== 'boolean') throw new Error('服务器没有返回有效的收藏状态');
                    card.isFavorite = detail.item.isFavorite; button.title = ''; refresh();
                    showToast('收藏状态已读取，可再次点击更改。'); return;
                }
                const result = await nativeRequest('mediaStationCatalog', 'set_favorite', ['set_favorite', JSON.stringify({ mediaId: card.id, favorite: card.isFavorite !== true })]);
                if (result.mediaId !== card.id || typeof result.isFavorite !== 'boolean') throw new Error('服务器没有返回有效的收藏状态');
                card.isFavorite = result.isFavorite;
                const sources = [homeData?.resume || [], homeData?.latest || [], ...(homeData?.latestSections || []).map((section) => section.items || []), ...[...libraryCache.values()].map((entry) => entry.items || [])];
                sources.flat().filter((item) => item.id === card.id).forEach((item) => { item.isFavorite = result.isFavorite; });
                seriesDetailCache.delete(seriesDetailCacheKey(card.id));
                if (currentView?.kind === 'favorites') void loadFavorites(currentView.data, { refresh: true });
                if (result.cacheWarning) showToast('收藏已保存，但本地缓存更新失败；请重新打开客户端核验。');
                document.querySelectorAll('[data-favorite-id]').forEach((node) => {
                    if (node.dataset.favoriteId === card.id) { node.textContent = card.isFavorite ? '✓ 已收藏' : '+ 收藏'; node.setAttribute('aria-pressed', String(card.isFavorite)); }
                });
            } catch (error) { showToast(`收藏操作失败：${friendlyError(error)}`); }
            finally { button.disabled = false; button.removeAttribute('aria-busy'); }
        });
        return button;
    }

    function renderFavorites(data) {
        replaceContent(); const header = element('div', 'page-header'); header.append(element('h1', '', '收藏')); content.append(header);
        if (data.items.length) {
            const grid = element('div', 'grid-view'); data.items.forEach((card, index) => grid.append(createMediaCard(card, { index, onClick: openDetail }))); content.append(grid);
        }
        if (data.loading) content.append(element('p', 'library-state', '正在读取收藏…'));
        else if (data.error) {
            const state = element('div', 'library-state'); state.append(element('p', '', data.error));
            const retry = element('button', 'secondary-command', '重新尝试'); retry.type = 'button'; retry.addEventListener('click', () => void loadFavorites(data)); state.append(retry); content.append(state);
        } else if (!data.items.length) content.append(element('p', 'empty-state', '暂无收藏作品'));
        else if (data.items.length < data.totalRecordCount) {
            const more = element('button', 'secondary-command favorite-more', '加载更多'); more.type = 'button'; more.addEventListener('click', () => void loadFavorites(data)); content.append(more);
        }
    }

    async function loadFavorites(data, { refresh = false } = {}) {
        if (data.loading && !refresh) return;
        const revision = data.requestRevision = (data.requestRevision || 0) + 1;
        if (refresh) { data.items = []; data.totalRecordCount = 0; }
        data.loading = true; data.error = '';
        if (currentView?.data === data) renderFavorites(data);
        try {
            const startIndex = data.items.length;
            const response = await nativeRequest('mediaStationCatalog', 'favorites', ['favorites', JSON.stringify({ startIndex, limit: libraryPageSize })]);
            const page = validateLibraryPage(response, startIndex);
            if (data.requestRevision !== revision || currentView?.data !== data) return;
            data.items.push(...page.items); data.totalRecordCount = page.totalRecordCount;
        } catch (error) {
            if (data.requestRevision === revision) data.error = `收藏加载失败：${friendlyError(error)}`;
        } finally {
            if (data.requestRevision === revision) {
                data.loading = false;
                if (currentView?.data === data) renderFavorites(data);
            }
        }
    }

    function renderLibraries(libraries) {
        replaceContent();
        const backdropCard = libraries.find((library) => library.backdropImage || library.landscapeImage || library.primaryImage);
        loadViewBackdrop(backdropCard && (backdropCard.backdropImage || backdropCard.landscapeImage || backdropCard.primaryImage), appBackdropRevision, currentView);
        const header = element('div', 'page-header');
        const back = element('button', 'back-button hero-carousel-previous');
        back.append(element('span', 'hero-carousel-chevron'));
        back.type = 'button'; back.title = '返回'; back.setAttribute('aria-label', '返回'); back.dataset.focusKey = 'back'; back.addEventListener('click', goBack);
        const copy = element('div', 'page-header-copy');
        copy.append(element('h1', '', '媒体库'), element('p', '', `${libraries.length} 个媒体库 · 选择一个开始浏览`));
        header.append(copy);
        const grid = element('div', 'grid-view library-grid');
        libraries.forEach((library, index) => grid.append(createMediaCard(library, { landscape: true, library: true, index, onClick: openLibrary })));
        content.append(header, grid);
    }

    async function openLibrary(library) {
        const captured = captureView();
        const preferences = libraryBrowsePreferences.get(library.id);
        const filter = { itemType: '', genre: '', sort: 'DateCreated', ...preferences?.filter };
        const cached = getLibraryCache(library, filter);
        if (cached) {
            const data = {
                library,
                filter,
                viewMode: preferences?.viewMode || 'grid',
                filterExpanded: { ...preferences?.filterExpanded },
                ...cached,
                items: cached.items.slice(),
                syncing: true,
                filterLoading: false,
                filterError: '',
                filterRevision: ++libraryFilterRevision,
            };
            setCurrentView({ kind: 'library', data });
            if (preferences?.scrollTop) restoreViewState({ scrollTop: preferences.scrollTop, rows: {}, focusKey: '' }, { restoreFocus: false });
            syncLibraryFirstPage(data);
            return;
        }
        const loadingView = beginLoadingView('library-loading', captured, renderLoading);
        try {
            const [response, filterResponse] = await Promise.all([
                nativeRequest('mediaStationCatalog', 'items', [
                    'items', JSON.stringify(libraryRequestParams({ library, filter }, 0)),
                ]),
                nativeRequest('mediaStationCatalog', 'filters', [
                    'filters', JSON.stringify({ parentId: library.id, collectionType: library.collectionType || null }),
                ]),
            ]);
            const page = validateLibraryPage(response, 0);
            const filters = validateLibraryFilters(filterResponse);
            if (currentView !== loadingView) return;
            const data = {
                library,
                filter,
                viewMode: preferences?.viewMode || 'grid',
                filterExpanded: { ...preferences?.filterExpanded },
                filters,
                ...page,
                syncing: false,
                isLoadingMore: false,
                filterLoading: false,
                filterError: '',
                filterRevision: ++libraryFilterRevision,
            };
            putLibraryCache(data);
            currentView = { kind: 'library', data, scrollTop: 0, rows: {}, focusKey: '' };
            renderCurrentView('forward');
        } catch (error) {
            if (restoreLoadingSource(loadingView, captured)) showToast(friendlyError(error));
        }
    }

    function libraryRequestParams(data, startIndex) {
        const params = { parentId: data.library.id, startIndex, limit: libraryPageSize };
        if (data.filter?.itemType) params.itemType = data.filter.itemType;
        if (data.filter?.genre) params.genre = data.filter.genre;
        params.sort = data.filter?.sort || 'DateCreated';
        return params;
    }

    function libraryFilterIdentity(filter) {
        return JSON.stringify([filter?.itemType || '', filter?.genre || '', filter?.sort || 'DateCreated']);
    }

    function isLibraryRequestCurrent(data, revision, filterIdentity) {
        return data.filterRevision === revision
            && libraryFilterIdentity(data.filter) === filterIdentity;
    }

    function validateLibraryFilters(filters) {
        if (!filters || !Array.isArray(filters.itemTypes) || !Array.isArray(filters.genres)) {
            throw new Error('媒体库筛选响应无效');
        }
        const itemTypes = [...new Set(filters.itemTypes.filter((value) => typeof value === 'string' && value))];
        const genres = [...new Set(filters.genres.filter((value) => typeof value === 'string' && value))];
        if (itemTypes.length !== filters.itemTypes.length || genres.length !== filters.genres.length) {
            throw new Error('媒体库筛选包含无效或重复值');
        }
        return { itemTypes, genres };
    }

    function mediaTypeLabel(type) {
        return ({ Movie: '电影', Series: '剧集', Video: '视频', MusicVideo: '音乐视频', BoxSet: '合集' })[type] || type;
    }

    function createFilterGroup({
        key,
        label,
        values,
        selected,
        expanded,
        onExpandedChange,
        onSelect,
        valueLabel = (value) => value,
    }) {
        const group = element('div', 'library-filter-group');
        group.append(element('span', 'library-filter-label', label));
        const container = element('div', 'library-filter-controls');
        const controls = element('div', 'library-filter-options');
        controls.id = `library-filter-options-${key}-${libraryFilterRevision}`;
        controls.setAttribute('role', 'group');
        controls.setAttribute('aria-label', label);
        ['', ...values].forEach((value) => {
            const button = element('button', 'library-filter-button', value ? valueLabel(value) : '全部');
            button.type = 'button';
            button.dataset.filterValue = value;
            button.dataset.focusKey = `filter:${label}:${value || 'all'}`;
            button.setAttribute('aria-pressed', String(value === selected));
            button.addEventListener('click', () => onSelect(value));
            controls.append(button);
        });
        const toggle = element('button', 'library-filter-toggle');
        toggle.type = 'button';
        toggle.hidden = true;
        toggle.dataset.focusKey = `filter:${label}:toggle`;
        toggle.setAttribute('aria-controls', controls.id);

        const revealSelected = () => {
            if (!group.isConnected || group.classList.contains('expanded')) return;
            const selectedButton = controls.querySelector('[aria-pressed="true"]');
            if (selectedButton) {
                controls.scrollLeft = Math.max(0, revealTarget(controls, selectedButton, 'x', 'nearest'));
            }
        };
        const setExpanded = (next, notify = true) => {
            const active = next === true;
            group.classList.toggle('expanded', active);
            toggle.textContent = active ? '收起' : '展开';
            toggle.setAttribute('aria-expanded', String(active));
            toggle.setAttribute('aria-label', `${active ? '收起' : '展开'}${label}筛选`);
            if (active) controls.scrollLeft = 0;
            else requestAnimationFrame(revealSelected);
            if (notify) onExpandedChange(active);
        };
        setExpanded(expanded, false);
        toggle.addEventListener('click', () => {
            setExpanded(!group.classList.contains('expanded'));
        });
        group._syncOverflow = () => {
            const buttons = [...controls.querySelectorAll('.library-filter-button')];
            const style = window.getComputedStyle(controls);
            const gap = Number.parseFloat(style.columnGap || style.gap) || 0;
            const requiredWidth = buttons.reduce(
                (total, button) => total + button.getBoundingClientRect().width,
                Math.max(0, buttons.length - 1) * gap,
            );
            const overflowing = requiredWidth > container.clientWidth + 0.5;
            toggle.hidden = !overflowing;
            group.classList.toggle('has-overflow', overflowing);
            if (!overflowing && group.classList.contains('expanded')) setExpanded(false);
            else if (overflowing && !group.classList.contains('expanded')) requestAnimationFrame(revealSelected);
        };
        container.append(controls, toggle);
        group.append(container);
        return group;
    }

    function renderLibraryFilters(data) {
        data.filterExpanded ||= { itemType: false, genre: false };
        const bar = element('section', 'library-filters');
        bar.setAttribute('aria-label', '媒体库筛选');
        if (data.filters.genres.length) {
            bar.append(createFilterGroup({
                key: 'genre',
                label: '题材',
                values: data.filters.genres,
                selected: data.filter.genre,
                expanded: data.filterExpanded.genre,
                onExpandedChange: (expanded) => { data.filterExpanded.genre = expanded; },
                onSelect: (genre) => applyLibraryFilter(data, { ...data.filter, genre }),
            }));
        }
        requestAnimationFrame(() => {
            if (!bar.isConnected) return;
            bar.querySelectorAll('.library-filter-group').forEach((group) => group._syncOverflow?.());
        });
        return bar;
    }

    function renderLibrary(data) {
        libraryPageObserver.disconnect();
        replaceContent();
        loadViewBackdrop(data.library.backdropImage || data.library.landscapeImage, appBackdropRevision, currentView);
        const header = element('div', 'page-header');
        const breadcrumb = element('nav', 'library-breadcrumb'); breadcrumb.setAttribute('aria-label', '库内导航');
        const directory = element('button', '', '媒体库'); directory.type = 'button'; directory.dataset.focusKey = 'back';
        directory.addEventListener('click', () => setCurrentView({ kind: 'libraries', data: homeData.libraries }));
        breadcrumb.append(directory, element('span', '', '›'), element('span', '', data.library.title));
        const back = element('button', 'back-button hero-carousel-previous');
        back.append(element('span', 'hero-carousel-chevron'));
        back.type = 'button'; back.title = '返回'; back.setAttribute('aria-label', '返回'); back.dataset.focusKey = 'back'; back.addEventListener('click', goBack);
        const cover = element('span', 'library-header-cover');
        const image = document.createElement('img'); image.alt = '';
        observeImage(image, data.library.landscapeImage || data.library.primaryImage, 360);
        cover.append(image); header.append(cover, element('h1', '', data.library.title));
        content.append(breadcrumb, header);
        if (data.filters.genres.length) content.append(renderLibraryFilters(data));
        const toolbar = element('div', 'library-toolbar');
        toolbar.append(element('span', '', data.filterLoading ? '正在筛选…' : data.totalRecordCount + ' 部作品'));
        const controls = element('div', 'library-toolbar-controls');
        const sort = document.createElement('select');
        sort.setAttribute('aria-label', '作品排序'); sort.dataset.focusKey = 'library:sort';
        [['DateCreated', '最近添加'], ['SortName', '名称'], ['ProductionYear', '发行年份'], ['CommunityRating', '评分']]
            .forEach(([value, label]) => { const option = document.createElement('option'); option.value = value; option.textContent = label; sort.append(option); });
        sort.value = data.filter.sort || 'DateCreated'; sort.disabled = Boolean(data.filterLoading);
        sort.addEventListener('change', () => applyLibraryFilter(data, { ...data.filter, sort: sort.value }));
        controls.append(sort);
        const views = element('div', 'library-view-switch'); views.setAttribute('role', 'group'); views.setAttribute('aria-label', '显示方式');
        [['grid', '网格'], ['list', '列表']].forEach(([mode, label]) => {
            const button = element('button'); button.type = 'button'; button.append(strokeIcon(mode));
            button.setAttribute('aria-label', label); button.title = label;
            button.setAttribute('aria-pressed', String((data.viewMode || 'grid') === mode));
            button.addEventListener('click', () => {
                data.viewMode = mode; content.querySelector('.library-items-grid')?.classList.toggle('list-view', mode === 'list');
                [...views.children].forEach((child) => child.setAttribute('aria-pressed', String(child === button)));
            }); views.append(button);
        });
        controls.append(views); toolbar.append(controls); content.append(toolbar);
        if (data.filterLoading) {
            content.append(element('div', 'library-state', '正在加载筛选结果…'));
            return;
        }
        if (data.filterError) {
            const state = element('div', 'library-state error-state');
            state.append(element('p', '', data.filterError));
            const retry = element('button', 'secondary-command', '重试');
            retry.type = 'button';
            retry.addEventListener('click', () => applyLibraryFilter(data, { ...data.filter }, true));
            state.append(retry);
            content.append(state);
            return;
        }
        if (!data.items.length) {
            content.append(element('div', 'empty-state', '当前筛选没有结果'));
            return;
        }
        const grid = element('div', 'grid-view library-items-grid' + (data.viewMode === 'list' ? ' list-view' : ''));
        data.items.forEach((card, index) => grid.append(createMediaCard(card, {
            index,
            onClick: openDetail,
            onFocus: updateBackdropForCard,
        })));
        content.append(grid);
        if (data.syncError) {
            content.append(element('div', 'library-sync-error', `同步失败，当前显示缓存内容：${data.syncError}`));
        }
        content.append(element('div', 'library-page-sentinel'));
        updateLibraryPaginationUi(data);
    }

    function updateBackdropForCard(card) {
        const revision = ++appBackdropRevision;
        loadViewBackdrop(card.backdropImage || card.landscapeImage || card.primaryImage, revision, currentView);
    }

    async function applyLibraryFilter(data, filter, force = false) {
        if (currentView?.kind !== 'library' || currentView.data !== data || data.filterLoading) return;
        if (!force && libraryFilterIdentity(filter) === libraryFilterIdentity(data.filter)) return;
        const revision = ++libraryFilterRevision;
        const filterIdentity = libraryFilterIdentity(filter);
        const focusKey = filter.sort !== data.filter.sort ? 'library:sort' : 'filter:题材:' + (filter.genre || 'all');
        data.filter = filter;
        data.filterRevision = revision;
        data.items = [];
        data.startIndex = 0;
        data.nextStartIndex = 0;
        data.totalRecordCount = 0;
        data.isLoadingMore = false;
        data.loadMoreError = '';
        data.syncError = '';
        data.syncing = false;
        data.filterLoading = true;
        data.filterError = '';
        content.scrollTop = 0;
        renderLibrary(data);
        requestAnimationFrame(() => focusElement([...content.querySelectorAll('[data-focus-key]')]
            .find((node) => node.dataset.focusKey === focusKey)));
        try {
            const requestParams = libraryRequestParams(data, 0);
            const response = await nativeRequest('mediaStationCatalog', 'items', [
                'items', JSON.stringify(requestParams),
            ]);
            const page = validateLibraryPage(response, 0);
            if (!isLibraryRequestCurrent(data, revision, filterIdentity)
                || currentView?.kind !== 'library' || currentView.data !== data) return;
            Object.assign(data, page, { filterLoading: false, filterError: '' });
            putLibraryCache(data);
            renderLibrary(data);
            requestAnimationFrame(() => focusElement([...content.querySelectorAll('[data-focus-key]')]
                .find((node) => node.dataset.focusKey === focusKey)));
        } catch (error) {
            if (!isLibraryRequestCurrent(data, revision, filterIdentity)
                || currentView?.kind !== 'library' || currentView.data !== data) return;
            data.filterLoading = false;
            data.filterError = `筛选加载失败：${friendlyError(error)}`;
            renderLibrary(data);
            requestAnimationFrame(() => focusElement([...content.querySelectorAll('[data-focus-key]')]
                .find((node) => node.dataset.focusKey === focusKey)));
        }
    }

    function updateLibraryPaginationUi(data) {
        libraryPageObserver.disconnect();
        if (currentView?.kind !== 'library' || currentView.data !== data) return;
        const sentinel = content.querySelector('.library-page-sentinel');
        if (!sentinel) return;
        sentinel.replaceChildren();
        if (data.nextStartIndex >= data.totalRecordCount) return;
        if (data.loadMoreError) {
            const retry = element('button', 'secondary-command', '重试加载');
            retry.type = 'button';
            retry.addEventListener('click', () => loadMoreLibrary(data));
            sentinel.append(element('span', '', data.loadMoreError), retry);
            return;
        }
        if (data.isLoadingMore) {
            sentinel.textContent = '正在加载…';
            return;
        }
        libraryPageObserver.observe(sentinel);
    }

    async function loadMoreLibrary(data) {
        if (data.isLoadingMore || data.syncing || data.nextStartIndex >= data.totalRecordCount) return;
        if (currentView?.kind !== 'library' || currentView.data !== data) return;
        const requestView = currentView;
        const requestRevision = data.filterRevision;
        const filterIdentity = libraryFilterIdentity(data.filter);
        const requestedStart = data.nextStartIndex;
        const requestParams = libraryRequestParams(data, requestedStart);
        data.isLoadingMore = true;
        data.loadMoreError = '';
        updateLibraryPaginationUi(data);
        try {
            const response = await nativeRequest('mediaStationCatalog', 'items', [
                'items',
                JSON.stringify(requestParams),
            ]);
            if (!isLibraryRequestCurrent(data, requestRevision, filterIdentity)) return;
            data.isLoadingMore = false;
            if (currentView !== requestView) return;
            const page = validateLibraryPage(response, requestedStart);
            const grid = content.querySelector('.grid-view');
            if (!grid) throw new Error('媒体库网格已不可用');
            appendLibraryPage(data, page);
            putLibraryCache(data);
            const firstIndex = data.items.length - page.items.length;
            page.items.forEach((card, index) => grid.append(createMediaCard(card, {
                index: firstIndex + index,
                onClick: openDetail,
                onFocus: updateBackdropForCard,
            })));
            updateLibraryPaginationUi(data);
        } catch (error) {
            if (!isLibraryRequestCurrent(data, requestRevision, filterIdentity)) return;
            data.isLoadingMore = false;
            if (currentView !== requestView) return;
            data.loadMoreError = friendlyError(error);
            updateLibraryPaginationUi(data);
            showToast(`继续加载失败：${data.loadMoreError}`);
        }
    }

    async function syncLibraryFirstPage(data) {
        const scrollTop = content.scrollTop;
        const requestRevision = data.filterRevision;
        const filterIdentity = libraryFilterIdentity(data.filter);
        const requestParams = libraryRequestParams(data, 0);
        let needsRender = false;
        try {
            const response = await nativeRequest('mediaStationCatalog', 'items', [
                'items',
                JSON.stringify(requestParams),
            ]);
            if (!isLibraryRequestCurrent(data, requestRevision, filterIdentity)) return;
            const page = validateLibraryPage(response, 0);
            const cachedPrefix = data.items.slice(0, page.items.length).map((item) => item.id);
            const refreshedIds = page.items.map((item) => item.id);
            const unchanged = data.totalRecordCount === page.totalRecordCount
                && cachedPrefix.length === refreshedIds.length
                && cachedPrefix.every((id, index) => id === refreshedIds[index]);
            if (!unchanged) {
                data.items = page.items.slice();
                needsRender = true;
            }
            data.startIndex = 0;
            data.totalRecordCount = page.totalRecordCount;
            data.nextStartIndex = unchanged ? data.items.length : page.nextStartIndex;
            data.syncing = false;
            data.syncError = '';
            data.loadMoreError = '';
            putLibraryCache(data);
        } catch (error) {
            if (!isLibraryRequestCurrent(data, requestRevision, filterIdentity)) return;
            data.syncing = false;
            data.syncError = friendlyError(error);
            console.error(`媒体库后台同步失败：${data.syncError}`);
        }
        if (currentView?.kind === 'library' && currentView.data === data) {
            if (needsRender || data.syncError) {
                renderLibrary(data);
                restoreViewState({ scrollTop, rows: {}, focusKey: '' });
            } else {
                updateLibraryPaginationUi(data);
            }
        }
    }

    function validateLibraryPage(page, requestedStart, label = '媒体库') {
        if (!page || !Array.isArray(page.items)) throw new Error(`${label}分页响应缺少项目列表`);
        const startIndex = Number(page.startIndex);
        const totalRecordCount = Number(page.totalRecordCount);
        const nextStartIndex = Number(page.nextStartIndex);
        if (!Number.isInteger(startIndex) || startIndex !== requestedStart) {
            throw new Error(`${label}分页起始位置与请求不一致`);
        }
        if (!Number.isInteger(totalRecordCount) || totalRecordCount < 0) {
            throw new Error(`${label}分页总数无效`);
        }
        if (!Number.isInteger(nextStartIndex) || nextStartIndex !== startIndex + page.items.length) {
            throw new Error(`${label}下一页位置无效`);
        }
        if (nextStartIndex > totalRecordCount || (nextStartIndex < totalRecordCount && page.items.length === 0)) {
            throw new Error(`${label}分页提前结束或超出总数`);
        }
        const ids = new Set();
        for (const item of page.items) {
            if (!item?.id || ids.has(item.id)) throw new Error(`${label}分页包含无效或重复项目`);
            ids.add(item.id);
        }
        return { items: page.items, startIndex, nextStartIndex, totalRecordCount };
    }

    function appendLibraryPage(data, page) {
        if (page.startIndex !== data.nextStartIndex) throw new Error('媒体库分页状态已变化，请重新进入媒体库');
        if (page.totalRecordCount !== data.totalRecordCount) throw new Error('媒体库内容已变化，请重新进入媒体库');
        const existingIds = new Set(data.items.map((item) => item.id));
        if (page.items.some((item) => existingIds.has(item.id))) {
            throw new Error('媒体库分页出现重复项目，请重新进入媒体库');
        }
        data.items.push(...page.items);
        data.nextStartIndex = page.nextStartIndex;
    }

    function libraryCacheKey(library, filter = {}) {
        return [session?.baseUrl || '', session?.userId || '', library.id, filter.itemType || '', filter.genre || '', filter.sort || 'DateCreated'].join('\n');
    }

    function getLibraryCache(library, filter) {
        const key = libraryCacheKey(library, filter);
        const cached = libraryCache.get(key);
        if (!cached) return null;
        libraryCache.delete(key);
        libraryCache.set(key, cached);
        return cached;
    }

    function putLibraryCache(data) {
        const key = libraryCacheKey(data.library, data.filter);
        libraryCache.delete(key);
        libraryCache.set(key, {
            items: data.items.slice(),
            startIndex: 0,
            nextStartIndex: data.nextStartIndex,
            totalRecordCount: data.totalRecordCount,
            filters: data.filters,
        });
        while (libraryCache.size > maximumLibraryCacheEntries) {
            libraryCache.delete(libraryCache.keys().next().value);
        }
    }

    function seriesDetailCacheKey(seriesId) {
        return [session?.baseUrl || '', session?.userId || '', seriesId].join('\n');
    }

    function getSeriesDetailCache(card) {
        if (card?.type !== 'Series' || !card.id) return null;
        const key = seriesDetailCacheKey(card.id);
        const cached = seriesDetailCache.get(key);
        if (!cached) return null;
        seriesDetailCache.delete(key);
        seriesDetailCache.set(key, cached);
        return cached;
    }

    function putSeriesDetailCache(detail) {
        if (detail?.item?.type !== 'Series' || !detail.item.id) return;
        const key = seriesDetailCacheKey(detail.item.id);
        seriesDetailCache.delete(key);
        seriesDetailCache.set(key, detail);
        while (seriesDetailCache.size > maximumSeriesDetailCacheEntries) {
            seriesDetailCache.delete(seriesDetailCache.keys().next().value);
        }
    }

    function normalizeSeasonEpisodes(detail, episodes) {
        return Array.isArray(episodes)
            ? episodes.map((episode) => ({
                ...episode,
                landscapeImage: episode.landscapeImage || detail.item.backdropImage || detail.item.landscapeImage || detail.item.primaryImage,
            }))
            : [];
    }

    function detailRefreshKey(detail) {
        return [session?.baseUrl || '', session?.userId || '', detail?.item?.id || ''].join('\n');
    }

    function refreshSeriesDetailInBackground(detail) {
        const seriesId = detail?.item?.id;
        if (!seriesId) return;
        const refreshKey = detailRefreshKey(detail);
        if (detailRefreshRequests.has(refreshKey)) return;
        detailRefreshRequests.add(refreshKey);
        const expectedSession = session;
        window.setTimeout(async () => {
            try {
                if (session !== expectedSession) return;
                const refreshed = await nativeRequest('mediaStationCatalog', 'detail', [
                    'detail', JSON.stringify({ mediaId: seriesId, refresh: true }),
                ], 60000);
                if (session !== expectedSession) return;
                refreshed.episodesBySeason = detail.episodesBySeason || {};
                refreshed.selectedSeasonId = detail.selectedSeasonId;
                putSeriesDetailCache(refreshed);
                if (currentView?.kind !== 'detail' || currentView.data !== detail) return;
                applySeriesDetailRefresh(detail, refreshed);
            } catch (error) {
                console.error(`剧集详情后台刷新失败：${friendlyError(error)}`);
            } finally {
                detailRefreshRequests.delete(refreshKey);
            }
        }, detailRefreshDelayMs);
    }

    function goHome() {
        if (!homeData) return;
        history.length = 0;
        setCurrentView({ kind: 'home', data: homeData }, false);
    }

    async function openDetail(card) {
        if (card.type === 'CollectionFolder') {
            await openLibrary(card);
            return;
        }
        const captured = captureView();
        const cached = getSeriesDetailCache(card);
        if (cached) {
            setCurrentView({ kind: 'detail', data: cached }, true, captured);
            refreshSeriesDetailInBackground(cached);
            return;
        }
        const loadingView = beginLoadingView('detail-loading', captured, renderDetailLoading);
        try {
            const detail = await nativeRequest('mediaStationCatalog', 'detail', ['detail', JSON.stringify({ mediaId: card.id })]);
            if (currentView !== loadingView) return;
            putSeriesDetailCache(detail);
            currentView = { kind: 'detail', data: detail, scrollTop: 0, rows: {}, focusKey: '' };
            renderCurrentView('forward');
            if (detail.cache?.status === 'hit') refreshSeriesDetailInBackground(detail);
        } catch (error) {
            if (restoreLoadingSource(loadingView, captured)) showToast(friendlyError(error));
        }
    }

    function renderDetailLoading() {
        replaceContent();
        const view = element('div', 'detail-view');
        const backdrop = element('div', 'detail-backdrop placeholder-art');
        backdrop.style.height = 'min(72vh, 720px)';
        const body = element('div', 'detail-content');
        const back = element('button', 'back-button hero-carousel-previous detail-back placeholder-back');
        back.append(element('span', 'hero-carousel-chevron'));
        body.append(back);
        const layout = element('div', 'detail-layout');
        const poster = element('div', 'detail-poster placeholder-art');
        const copy = element('div', 'detail-copy');
        copy.append(
            element('div', 'placeholder-title detail-title-ph'),
            element('div', 'placeholder-subtitle detail-meta-ph'),
            element('div', 'placeholder-subtitle detail-overview-ph'),
        );
        layout.append(poster, copy);
        body.append(layout);
        view.append(backdrop, body);
        view.setAttribute('aria-hidden', 'true');
        content.append(view);
    }

    function renderDetail(detail) {
        const card = detail.item;
        replaceContent();
        const view = element('article', 'detail-view');
        view.dataset.seriesId = card.type === 'Series' ? card.id : '';
        const backdrop = element('div', 'detail-backdrop');
        const backdropImage = element('span', 'detail-backdrop-image');
        backdrop.append(backdropImage);
        const body = element('div', 'detail-content');
        const back = element('button', 'back-button hero-carousel-previous detail-back');
        back.append(element('span', 'hero-carousel-chevron'));
        const previous = history.at(-1);
        const backLabel = previous?.kind === 'library' ? `返回${previous.data.library.title}` : previous?.kind === 'home' ? '返回首页' : '返回';
        back.append(element('span', '', backLabel));
        back.type = 'button'; back.title = '返回'; back.setAttribute('aria-label', '返回'); back.dataset.focusKey = 'back'; back.addEventListener('click', goBack);
        const layout = element('div', 'detail-layout');
        const poster = element('div', 'detail-poster');
        const posterFallback = element('span', 'art-fallback', initials(card.title));
        const posterImage = document.createElement('img');
        posterImage.alt = '';
        posterImage.decoding = 'async';
        poster.append(posterFallback, posterImage);
        const posterRef = card.primaryImage || card.landscapeImage;
        observeImage(posterImage, posterRef, imageWidthFor(posterRef, false));
        const copy = element('div', 'detail-copy');
        const title = element('h1', 'detail-title', card.title);
        title.dataset.detailField = 'title';
        copy.append(title);
        if (card.logoImage) {
            const logo = document.createElement('img');
            logo.className = 'detail-logo';
            logo.alt = card.title;
            logo.hidden = true;
            copy.prepend(logo);
            requestImage(card.logoImage, 720).then((src) => {
                if (!logo.isConnected || currentView?.data !== detail) return;
                logo.onload = () => {
                    if (!logo.isConnected || currentView?.data !== detail) return;
                    logo.hidden = false;
                    title.classList.add('logo-title-fallback');
                };
                logo.onerror = () => {
                    logo.hidden = true;
                    title.classList.remove('logo-title-fallback');
                    console.error(`Logo 解码失败：${card.title}`);
                };
                logo.src = src;
            }).catch((error) => {
                if (currentView?.data === detail) console.error(`Logo 加载失败：${friendlyError(error)}`);
            });
        }
        copy.append(createDetailMeta(detail));
        if (card.overview) {
            copy.append(createDetailOverview(detail));
        }
        const actions = element('div', 'detail-actions');
        if (card.type === 'Series') actions.dataset.seriesId = card.id;
        const playTarget = card.playable ? card : (pickContinueEpisode(detail.episodes) || detail.episodes?.[0]);
        if (playTarget?.playable) appendDetailPlayAction(actions, playTarget, detail);
        actions.append(createFavoriteButton(card));
        if (card.type === 'Series' || playTarget?.playable) copy.append(actions);
        if (card.genres?.length) {
            copy.append(createDetailGenres(detail));
        }
        layout.append(poster, copy);
        const flowing = element('div', 'detail-flow');
        [...copy.children].filter((node) => node.matches('.detail-overview, .detail-actions, .tag-list')).forEach((node) => flowing.append(node));
        copy.append(flowing);
        body.append(back, layout);
        view.append(backdrop, body);
        const peopleHost = element('div', 'detail-people');
        peopleHost.dataset.detailField = 'people';
        if (detail.people?.length) peopleHost.append(createPeopleSection(detail.people));
        if (card.type === 'Series' && detail.seasons?.length) view.append(createEpisodes(detail, actions));
        view.append(peopleHost);
        content.append(view);
        const ref = card.backdropImage || card.landscapeImage;
        const backdropRevision = appBackdropRevision;
        loadViewBackdrop(ref, backdropRevision, currentView);
        if (ref) requestImage(ref, 1600).then((src) => {
            if (!backdropImage.isConnected || currentView?.data !== detail) return;
            backdropImage.style.backgroundImage = `url("${src}")`;
            requestAnimationFrame(() => {
                if (backdropImage.isConnected && currentView?.data === detail) backdropImage.classList.add('image-ready');
            });
        }).catch((error) => console.error(`详情背景加载失败：${friendlyError(error)}`));
    }

    function replaceDetailField(detail, field, replacement) {
        const selector = `[data-detail-field="${field}"]`;
        const existing = content.querySelector(selector);
        if (!existing) return false;
        existing.replaceWith(replacement);
        return true;
    }

    function createDetailMeta(detail) {
        const card = detail.item;
        const meta = element('div', 'hero-meta');
        meta.dataset.detailField = 'meta';
        const seriesMeta = card.type === 'Series'
            ? [
                Number.isFinite(detail.seasonCount) && detail.seasonCount > 0 ? `${detail.seasonCount} 季` : '',
                Number.isFinite(detail.episodeCount) && detail.episodeCount > 0 ? `${detail.episodeCount} 集` : '',
            ]
            : [];
        // 片源类型跟随所选版本，切换片源后这里显示的就是即将播放的版本。
        const { options, selected } = detailSourceState(detail);
        const dynamicRange = selected?.dynamicRange || card.dynamicRange;
        [card.communityRating ? `★ ${card.communityRating.toFixed(1)}` : '', card.year, ...seriesMeta, formatDuration(card.durationMs), mediaTypeLabel(card.type), card.officialRating, dynamicRange]
            .filter(Boolean).forEach((value) => meta.append(element('span', value.toString().startsWith('★') ? 'rating' : '', value)));
        if (options.length > 1) {
            const switchButton = element('button', 'meta-switch', '切换片源');
            switchButton.type = 'button';
            switchButton.title = '选择片源版本';
            switchButton.dataset.focusKey = `sources:${card.id}`;
            switchButton.setAttribute('aria-haspopup', 'dialog');
            switchButton.addEventListener('click', () => openSourcePicker(detail, switchButton));
            meta.append(switchButton);
        }
        return meta;
    }

    function createDetailOverview(detail) {
        const overview = element('button', 'detail-overview overview-preview', detail.item.overview);
        overview.type = 'button';
        overview.title = '查看完整简介';
        overview.dataset.detailField = 'overview';
        overview.dataset.focusKey = `overview:${detail.item.id}`;
        overview.addEventListener('click', () => openOverview(detail.item, overview));
        return overview;
    }

    function createDetailGenres(detail) {
        const tags = element('div', 'tag-list');
        tags.dataset.detailField = 'genres';
        detail.item.genres?.slice(0, 8).forEach((genre) => tags.append(element('span', 'tag', genre)));
        return tags;
    }

    function applySeriesDetailRefresh(detail, refreshed) {
        const previousItem = detail.item;
        const previousMeta = JSON.stringify({
            year: previousItem?.year,
            seasonCount: detail.seasonCount,
            episodeCount: detail.episodeCount,
            durationMs: previousItem?.durationMs,
            officialRating: previousItem?.officialRating,
            communityRating: previousItem?.communityRating,
            dynamicRange: previousItem?.dynamicRange,
        });
        const previousPeople = JSON.stringify(detail.people || []);
        const previousGenres = JSON.stringify(previousItem?.genres || []);
        const previousOverview = previousItem?.overview || '';
        const previousEpisodes = detail.episodes;
        Object.assign(detail, refreshed);
        detail.episodesBySeason = refreshed.episodesBySeason || {};
        detail.selectedSeasonId = refreshed.selectedSeasonId;
        detail.episodes = previousEpisodes;
        if (detail.item?.id !== previousItem?.id) return;
        const current = currentView;
        if (current?.kind !== 'detail' || current.data !== detail) return;
        const view = content.querySelector(`.detail-view[data-series-id="${detail.item.id}"]`);
        if (!view) return;

        const title = view.querySelector('[data-detail-field="title"]');
        if (title && previousItem?.title !== detail.item.title) title.textContent = detail.item.title;
        const nextMeta = JSON.stringify({
            year: detail.item.year,
            seasonCount: detail.seasonCount,
            episodeCount: detail.episodeCount,
            durationMs: detail.item.durationMs,
            officialRating: detail.item.officialRating,
            communityRating: detail.item.communityRating,
            dynamicRange: detail.item.dynamicRange,
        });
        if (previousMeta !== nextMeta) replaceDetailField(detail, 'meta', createDetailMeta(detail));
        const existingOverview = view.querySelector('[data-detail-field="overview"]');
        if (detail.item.overview) {
            if (existingOverview && previousOverview !== detail.item.overview) {
                replaceDetailField(detail, 'overview', createDetailOverview(detail));
            } else if (!existingOverview) {
                view.querySelector('.detail-actions')?.before(createDetailOverview(detail));
            }
        } else {
            existingOverview?.remove();
        }
        const nextGenres = JSON.stringify(detail.item.genres || []);
        const existingGenres = view.querySelector('[data-detail-field="genres"]');
        if (detail.item.genres?.length) {
            if (existingGenres && previousGenres !== nextGenres) {
                replaceDetailField(detail, 'genres', createDetailGenres(detail));
            } else if (!existingGenres) {
                view.querySelector('.detail-actions')?.after(createDetailGenres(detail));
            }
        } else {
            existingGenres?.remove();
        }

        const peopleHost = view.querySelector('[data-detail-field="people"]');
        if (peopleHost && previousPeople !== JSON.stringify(detail.people || [])) {
            peopleHost.replaceChildren(...(detail.people?.length ? [createPeopleSection(detail.people)] : []));
        }
        const episodesRoot = view.querySelector('[data-detail-field="episodes"]');
        if (episodesRoot) {
            episodesRoot._refreshEpisodes?.(detail);
        } else if (detail.seasons?.length) {
            view.append(createEpisodes(detail));
        }
    }

    function createPeopleSection(people) {
        const uniquePeople = [...new Map(people.filter((person) => person?.id && person?.name).map((person) => [person.id, person])).values()];
        const section = element('section', 'people-section');
        const heading = element('div', 'people-heading');
        heading.append(element('h2', '', '演职员'));
        const controls = element('div', 'people-heading-controls');
        heading.append(controls);
        const row = element('div', 'media-row people-row');
        row.dataset.rowKey = 'people';
        row.dataset.rowIndex = '0';
        uniquePeople.forEach((person, index) => {
            const button = element('button', 'media-card person-card');
            button.type = 'button';
            button.dataset.cardIndex = String(index);
            button.dataset.focusKey = `person:${person.id}`;
            const portrait = element('span', 'person-portrait');
            const fallback = element('span', 'art-fallback', initials(person.name));
            const image = document.createElement('img');
            image.alt = '';
            image.decoding = 'async';
            portrait.append(fallback, image);
            observeImage(image, person.primaryImage, 260);
            button.append(
                portrait,
                element('span', 'card-title', person.name),
                element('span', 'card-subtitle', [person.role, person.type].filter(Boolean).join(' · ')),
            );
            button.addEventListener('focus', () => {
                if (person.primaryImage) updateBackdropForCard({ primaryImage: person.primaryImage });
            });
            button.addEventListener('click', () => openPerson(person));
            row.append(button);
        });
        section.append(heading, createRowCarousel(row, '演职员', controls));
        return section;
    }

    async function openPerson(person) {
        const captured = captureView();
        const revision = ++personPageRevision;
        const loadingView = beginLoadingView('person-loading', captured, renderLoading);
        try {
            const response = await nativeRequest('mediaStationCatalog', 'person_items', [
                'person_items', JSON.stringify({ personId: person.id, startIndex: 0, limit: libraryPageSize }),
            ]);
            if (revision !== personPageRevision || currentView !== loadingView) return;
            const page = validateLibraryPage(response, 0, '演员作品');
            const data = { person, ...page, isLoadingMore: false, loadMoreError: '' };
            currentView = { kind: 'person', data, scrollTop: 0, rows: {}, focusKey: '' };
            renderCurrentView('forward');
        } catch (error) {
            if (revision !== personPageRevision) return;
            if (restoreLoadingSource(loadingView, captured)) showToast(friendlyError(error));
        }
    }

    function renderPerson(data) {
        personPageObserver.disconnect();
        replaceContent();
        loadViewBackdrop(data.person.primaryImage, appBackdropRevision, currentView);
        const header = element('div', 'page-header person-page-header');
        const back = element('button', 'back-button hero-carousel-previous');
        back.append(element('span', 'hero-carousel-chevron'));
        back.type = 'button';
        back.title = '返回';
        back.setAttribute('aria-label', '返回');
        back.dataset.focusKey = 'person:back';
        back.addEventListener('click', goBack);
        const identity = element('div', 'person-identity');
        if (data.person.primaryImage) {
            const portrait = element('span', 'person-page-portrait');
            const image = document.createElement('img');
            image.alt = '';
            image.decoding = 'async';
            portrait.append(element('span', 'art-fallback', initials(data.person.name)), image);
            observeImage(image, data.person.primaryImage, 260);
            identity.append(portrait);
        }
        const copy = element('div');
        copy.append(element('h1', '', data.person.name));
        const role = [data.person.role, data.person.type].filter(Boolean).join(' · ');
        if (role) copy.append(element('p', '', role));
        identity.append(copy);
        header.append(back, identity);
        content.append(header);
        if (data.items.length) {
            const count = element('div', 'person-results-heading', `${data.totalRecordCount} 部相关作品`);
            const grid = element('div', 'grid-view person-grid');
            data.items.forEach((card, index) => grid.append(createMediaCard(card, {
                index,
                onClick: openDetail,
                onFocus: updateBackdropForCard,
            })));
            content.append(count, grid);
        } else {
            content.append(element('div', 'empty-state', '没有找到该演职员的相关作品'));
        }
        content.append(element('div', 'person-page-sentinel'));
        updatePersonPaginationUi(data);
    }

    function updatePersonPaginationUi(data) {
        personPageObserver.disconnect();
        if (currentView?.kind !== 'person' || currentView.data !== data) return;
        const sentinel = content.querySelector('.person-page-sentinel');
        if (!sentinel) return;
        sentinel.replaceChildren();
        if (data.nextStartIndex >= data.totalRecordCount) return;
        if (data.loadMoreError) {
            const retry = element('button', 'secondary-command', '重试加载');
            retry.type = 'button';
            retry.addEventListener('click', () => loadMorePerson(data));
            sentinel.append(element('span', '', data.loadMoreError), retry);
        } else if (data.isLoadingMore) {
            sentinel.textContent = '正在加载…';
        } else {
            personPageObserver.observe(sentinel);
        }
    }

    async function loadMorePerson(data) {
        if (data.isLoadingMore || data.nextStartIndex >= data.totalRecordCount) return;
        if (currentView?.kind !== 'person' || currentView.data !== data) return;
        const requestView = currentView;
        data.isLoadingMore = true;
        data.loadMoreError = '';
        updatePersonPaginationUi(data);
        const requestedStart = data.nextStartIndex;
        try {
            const response = await nativeRequest('mediaStationCatalog', 'person_items', [
                'person_items', JSON.stringify({ personId: data.person.id, startIndex: requestedStart, limit: libraryPageSize }),
            ]);
            data.isLoadingMore = false;
            if (currentView !== requestView) return;
            const page = validateLibraryPage(response, requestedStart, '演员作品');
            const existingIds = new Set(data.items.map((item) => item.id));
            if (page.totalRecordCount !== data.totalRecordCount || page.items.some((item) => existingIds.has(item.id))) {
                throw new Error('演员作品分页状态已变化，请重新进入');
            }
            const firstIndex = data.items.length;
            data.items.push(...page.items);
            data.nextStartIndex = page.nextStartIndex;
            const grid = content.querySelector('.person-grid');
            if (!grid) throw new Error('演员作品网格已不可用');
            page.items.forEach((card, index) => grid.append(createMediaCard(card, {
                index: firstIndex + index,
                onClick: openDetail,
                onFocus: updateBackdropForCard,
            })));
            updatePersonPaginationUi(data);
        } catch (error) {
            data.isLoadingMore = false;
            if (currentView !== requestView) return;
            data.loadMoreError = friendlyError(error);
            updatePersonPaginationUi(data);
        }
    }

    function createEpisodes(detail, detailActions) {
        const root = element('section', 'episodes');
        root.dataset.detailField = 'episodes';
        let seasons = normalizedSeasons(detail.seasons);
        if (!seasons.length) return root;
        const resumableEpisode = homeData?.resume?.find((episode) => (
            episode?.seriesId === detail.item.id && episode.resumePositionMs > 0
        ));
        let selectedSeasonId = seasons.some((season) => season.id === detail.selectedSeasonId)
            ? detail.selectedSeasonId
            : seasons.some((season) => season.id === resumableEpisode?.seasonId)
                ? resumableEpisode.seasonId
            : seasons.find((season) => season.indexNumber > 0)?.id || seasons[0].id;
        const heading = element('div', 'episodes-heading');
        heading.append(element('h2', '', '选集'));
        const headingControls = element('div', 'episodes-heading-controls');
        const pagingControls = element('div', 'episodes-paging-controls');
        const seasonPicker = element('div', 'season-picker');
        const seasonMenuButton = element('button', 'season-picker-button');
        seasonMenuButton.type = 'button';
        seasonMenuButton.dataset.focusKey = `season-picker:${detail.item.id}`;
        seasonMenuButton.setAttribute('aria-haspopup', 'listbox');
        seasonMenuButton.setAttribute('aria-expanded', 'false');
        seasonMenuButton.append(element('span', 'season-picker-label'));
        const seasonMenu = element('div', 'season-picker-menu');
        seasonMenu.setAttribute('role', 'listbox');
        seasonMenu.setAttribute('aria-hidden', 'true');
        seasonPicker.append(seasonMenuButton, seasonMenu);
        const stage = element('div', 'episode-stage');
        const seasonOptions = new Map();
        const loadingSeasonIds = new Set();
        const refreshingSeasonIds = new Set();
        let renderedSeasonId = null;
        let renderedEpisodes = [];

        const seasonLabel = (season) => {
            const count = Number.isFinite(season.episodeCount) && season.episodeCount > 0
                ? `${season.episodeCount} 集`
                : '';
            return [episodeSeasonLabel(season.indexNumber), count].filter(Boolean).join(' · ');
        };
        const dismissSeasonMenu = (event) => {
            if (!seasonPicker.isConnected) {
                document.removeEventListener('pointerdown', dismissSeasonMenu, true);
                return;
            }
            if (!seasonPicker.contains(event.target)) {
                closeSeasonMenu();
                ensureSelectedSeasonLoaded();
            }
        };
        const closeSeasonMenu = (returnFocus = false) => {
            if (!seasonPicker.classList.contains('is-open')) return;
            seasonPicker.classList.remove('is-open');
            seasonMenu.setAttribute('aria-hidden', 'true');
            seasonMenuButton.setAttribute('aria-expanded', 'false');
            document.removeEventListener('pointerdown', dismissSeasonMenu, true);
            if (returnFocus) focusElement(seasonMenuButton);
        };
        const syncSeasonPicker = () => {
            const selected = seasons.find((season) => season.id === selectedSeasonId);
            seasonMenuButton.querySelector('.season-picker-label').textContent = selected
                ? seasonLabel(selected)
                : '选择季';
            seasonMenu.querySelectorAll('.season-picker-option').forEach((option) => {
                const active = option.dataset.seasonId === selectedSeasonId;
                option.setAttribute('aria-selected', String(active));
                option.classList.toggle('selected', active);
            });
        };
        const selectSeasonOption = (option) => {
            if (!option || option.dataset.seasonId === selectedSeasonId) return false;
            const previous = seasonMenu.querySelector('.season-picker-option.selected');
            selectedSeasonId = option.dataset.seasonId;
            detail.selectedSeasonId = selectedSeasonId;
            if (previous) {
                previous.setAttribute('aria-selected', 'false');
                previous.classList.remove('selected');
            }
            option.setAttribute('aria-selected', 'true');
            option.classList.add('selected');
            const selected = seasons.find((season) => season.id === selectedSeasonId);
            seasonMenuButton.querySelector('.season-picker-label').textContent = selected
                ? seasonLabel(selected)
                : '选择季';
            return true;
        };
        const openSeasonMenu = () => {
            if (seasonPicker.classList.contains('is-open')) return;
            syncSeasonPicker();
            const selected = seasonMenu.querySelector('.season-picker-option.selected')
                || seasonMenu.querySelector('.season-picker-option');
            selected?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
            seasonPicker.classList.add('is-open');
            seasonMenu.setAttribute('aria-hidden', 'false');
            seasonMenuButton.setAttribute('aria-expanded', 'true');
            document.addEventListener('pointerdown', dismissSeasonMenu, true);
        };
        const createSeasonOption = (season) => {
            const option = element('button', 'season-picker-option');
            option.type = 'button';
            option.tabIndex = -1;
            option.dataset.seasonId = season.id;
            option.setAttribute('role', 'option');
            option.setAttribute('aria-label', seasonLabel(season));
            option.title = seasonLabel(season);
            option.append(element('span', 'season-picker-option-label', episodeSeasonLabel(season.indexNumber)));
            option.addEventListener('click', () => {
                selectSeasonOption(option);
                closeSeasonMenu();
                ensureSelectedSeasonLoaded();
            });
            return option;
        };
        const syncSeasonDirectory = (nextSeasons) => {
            seasons = normalizedSeasons(nextSeasons);
            const seasonIds = new Set(seasons.map((season) => season.id));
            for (const [seasonId, option] of seasonOptions) {
                if (seasonIds.has(seasonId)) continue;
                option.remove();
                seasonOptions.delete(seasonId);
            }
            for (const season of seasons) {
                let option = seasonOptions.get(season.id);
                if (!option) {
                    option = createSeasonOption(season);
                    seasonOptions.set(season.id, option);
                }
                const label = seasonLabel(season);
                option.setAttribute('aria-label', label);
                option.title = label;
                option.querySelector('.season-picker-option-label').textContent = episodeSeasonLabel(season.indexNumber);
                seasonMenu.append(option);
            }
            if (!seasonIds.has(selectedSeasonId)) {
                selectedSeasonId = seasons.find((season) => season.indexNumber > 0)?.id || seasons[0]?.id || null;
                detail.selectedSeasonId = selectedSeasonId;
            }
            syncSeasonPicker();
            if (seasons.length > 1) {
                if (!seasonPicker.isConnected) headingControls.prepend(seasonPicker);
            } else {
                closeSeasonMenu();
                seasonPicker.remove();
            }
        };
        syncSeasonDirectory(seasons);
        syncSeasonPicker();

        const renderSeason = (seasonId, episodes) => {
            renderedSeasonId = seasonId;
            renderedEpisodes = episodes;
            syncSeasonPicker();
            const cards = episodes.slice().sort((left, right) => (left.indexNumber || 0) - (right.indexNumber || 0));
            const grid = element('div', 'grid-view episodes-grid');
            cards.forEach((card, index) => grid.append(createMediaCard(card, {
                landscape: true, episodePicker: true, omitSeriesName: true, index,
                onClick: (item) => startPlayback(item, item.resumePositionMs),
            })));
            pagingControls.replaceChildren();
            stage.replaceChildren(grid.childElementCount ? grid : element('div', 'episode-stage-state', '这一季暂无剧集'));
        };

        const setDetailPlayAction = (episodes) => {
            // 与标准 Emby NextUp 一致：最近在看优先；首页“继续观看”列表
            // 只是次要回退（条目被移出最近观看时不能影响剧集页定位）。
            const target = pickContinueEpisode(episodes)
                || episodes.find((episode) => episode.id === resumableEpisode?.id)
                || episodes[0];
            if (!target?.playable) return;
            detailActions.querySelector('.primary-command')?.remove();
            appendDetailPlayAction(detailActions, target, detail);
        };

        const renderLoading = () => {
            stage.replaceChildren(element('div', 'episode-stage-state', '正在读取剧集...'));
        };
        const renderError = (error) => {
            const retry = element('button', 'secondary-command', '重试');
            retry.type = 'button';
            retry.addEventListener('click', () => loadSeason(selectedSeasonId));
            stage.replaceChildren(element('div', 'episode-stage-state', friendlyError(error)), retry);
        };
        const refreshSeasonInBackground = (seasonId) => {
            if (refreshingSeasonIds.has(seasonId)) return;
            refreshingSeasonIds.add(seasonId);
            const expectedSession = session;
            window.setTimeout(async () => {
                try {
                    if (session !== expectedSession) return;
                    const result = await nativeRequest('mediaStationCatalog', 'series_episodes', [
                        'series_episodes', JSON.stringify({ seriesId: detail.item.id, seasonId, refresh: true }),
                    ], 60000);
                    if (session !== expectedSession) return;
                    const episodes = normalizeSeasonEpisodes(detail, result.episodes);
                    detail.episodesBySeason ||= {};
                    detail.episodesBySeason[seasonId] = episodes;
                    if (selectedSeasonId !== seasonId || renderedSeasonId !== seasonId) return;
                    if (JSON.stringify(renderedEpisodes) === JSON.stringify(episodes)) return;
                    detail.episodes = episodes;
                    renderSeason(seasonId, episodes);
                    setDetailPlayAction(episodes);
                } catch (error) {
                    console.error(`剧集列表后台刷新失败：${friendlyError(error)}`);
                } finally {
                    refreshingSeasonIds.delete(seasonId);
                }
            }, detailRefreshDelayMs);
        };
        const loadSeason = async (seasonId) => {
            const season = seasons.find((candidate) => candidate.id === seasonId);
            if (!season) return;
            const cached = detail.episodesBySeason?.[seasonId];
            if (Array.isArray(cached)) {
                renderSeason(seasonId, cached);
                setDetailPlayAction(cached);
                refreshSeasonInBackground(seasonId);
                return;
            }
            if (loadingSeasonIds.has(seasonId)) return;
            loadingSeasonIds.add(seasonId);
            renderLoading();
            try {
                const result = await nativeRequest('mediaStationCatalog', 'series_episodes', [
                    'series_episodes', JSON.stringify({ seriesId: detail.item.id, seasonId }),
                ]);
                if (currentView?.kind !== 'detail' || currentView.data !== detail || selectedSeasonId !== seasonId) return;
                const episodes = normalizeSeasonEpisodes(detail, result.episodes);
                detail.episodesBySeason ||= {};
                detail.episodesBySeason[seasonId] = episodes;
                detail.episodes = episodes;
                renderSeason(seasonId, episodes);
                setDetailPlayAction(episodes);
                if (result.cache?.status === 'hit') refreshSeasonInBackground(seasonId);
            } catch (error) {
                if (currentView?.kind === 'detail' && currentView.data === detail && selectedSeasonId === seasonId) renderError(error);
            } finally {
                loadingSeasonIds.delete(seasonId);
            }
        };
        const ensureSelectedSeasonLoaded = () => {
            if (renderedSeasonId !== selectedSeasonId) loadSeason(selectedSeasonId);
        };

        seasonMenuButton.addEventListener('click', () => {
            if (!seasonPicker.classList.contains('is-open')) openSeasonMenu();
            else {
                closeSeasonMenu();
                ensureSelectedSeasonLoaded();
            }
        });
        if (seasons.length > 1) headingControls.append(seasonPicker);
        headingControls.append(pagingControls);
        heading.append(headingControls);
        root.append(heading, stage);
        loadSeason(selectedSeasonId);
        root._refreshEpisodes = (refreshedDetail) => {
            const previousSelectedSeasonId = selectedSeasonId;
            syncSeasonDirectory(refreshedDetail.seasons);
            const currentSeasonId = selectedSeasonId;
            if (!currentSeasonId) {
                stage.replaceChildren();
                return;
            }
            if (currentSeasonId !== previousSelectedSeasonId || renderedSeasonId !== currentSeasonId) {
                loadSeason(currentSeasonId);
                return;
            }
            const currentEpisodes = refreshedDetail.episodesBySeason?.[currentSeasonId];
            if (!Array.isArray(currentEpisodes) || renderedSeasonId !== currentSeasonId) return;
            const episodes = normalizeSeasonEpisodes(refreshedDetail, currentEpisodes);
            refreshedDetail.episodesBySeason[currentSeasonId] = episodes;
            if (JSON.stringify(renderedEpisodes) === JSON.stringify(episodes)) return;
            refreshedDetail.episodes = episodes;
            renderSeason(currentSeasonId, episodes);
            setDetailPlayAction(episodes);
        };
        return root;
    }

    function normalizedSeasons(value) {
        return Array.isArray(value)
            ? value.filter((season) => season?.id && Number.isFinite(season.indexNumber))
                .slice()
                .sort((left, right) => left.indexNumber - right.indexNumber)
            : [];
    }

    // 同一影片的多个片源版本（服务端 MediaSources）。版本之间没有独立名称，
    // 只能用分辨率、动态范围、编码、容器与体积区分。
    function detailVersionOptions(detail, target) {
        if (target?.type !== 'Movie') return [];
        return Array.isArray(detail?.sources) ? detail.sources.filter((source) => source?.id) : [];
    }

    function versionResolutionLabel(source) {
        const height = Number(source?.height) || 0;
        const width = Number(source?.width) || 0;
        if (height >= 2000 || width >= 3800) return '4K';
        if (height >= 1000 || width >= 1800) return '1080p';
        if (height >= 700 || width >= 1200) return '720p';
        if (height > 0) return `${height}p`;
        // 只有宽度时无法给出可靠档位，交给容器名兜底。
        return '';
    }

    function versionCodecLabel(codec) {
        const normalized = String(codec || '').trim().toLowerCase();
        if (['hevc', 'h265', 'h.265'].includes(normalized)) return 'HEVC';
        if (['h264', 'avc', 'h.264'].includes(normalized)) return 'H.264';
        if (normalized === 'av1') return 'AV1';
        if (normalized === 'vp9') return 'VP9';
        if (normalized === 'mpeg2video') return 'MPEG-2';
        return normalized ? normalized.toUpperCase() : '';
    }

    function versionRangeLabel(source) {
        const range = String(source?.dynamicRange || '').trim();
        if (/dolby\s*vision|dovi/i.test(range)) return '杜比视界';
        if (/hdr10\+/i.test(range)) return 'HDR10+';
        if (/hdr10/i.test(range)) return 'HDR10';
        if (/hlg/i.test(range)) return 'HLG';
        if (/^hdr$/i.test(range)) return 'HDR';
        return '';
    }

    function versionSizeLabel(bytes) {
        const value = Number(bytes) || 0;
        if (value <= 0) return '';
        const gigabytes = value / (1024 ** 3);
        if (gigabytes >= 1) return `${gigabytes.toFixed(gigabytes >= 10 ? 0 : 1)} GB`;
        const megabytes = value / (1024 ** 2);
        return megabytes >= 1 ? `${Math.round(megabytes)} MB` : '';
    }

    function detailVersionTitle(source, index) {
        const primary = [versionResolutionLabel(source), versionRangeLabel(source)].filter(Boolean).join(' ');
        if (primary) return primary;
        const container = String(source?.container || '').trim().toUpperCase();
        return container || `版本 ${index + 1}`;
    }

    function detailVersionMeta(source) {
        return [
            versionCodecLabel(source?.videoCodec),
            String(source?.container || '').trim().toUpperCase(),
            versionSizeLabel(source?.sizeBytes),
            source?.isRemote ? '云盘' : '本地',
            source?.supportsDirectPlay === false ? '不可直连' : '',
        ].filter(Boolean).join(' · ');
    }

    /// 当前选中的片源版本。服务端已按该用户偏好排序，首个可直连版本即默认值。
    function detailSourceState(detail) {
        const versions = detailVersionOptions(detail, detail?.item);
        const options = versions.length > 1 ? versions : [];
        const playable = options.filter((source) => source.supportsDirectPlay !== false);
        if (!playable.length) return { options, playable, selected: null };
        const selected = playable.find((source) => source.id === detail.selectedSourceId) || playable[0];
        detail.selectedSourceId = selected.id;
        return { options, playable, selected };
    }

    function createSourceOption(source, index, selected, onSelect) {
        const option = element('button', 'track-option');
        option.type = 'button';
        option.setAttribute('role', 'radio');
        option.setAttribute('aria-checked', String(selected));
        option.dataset.sourceId = source.id;
        option.disabled = source.supportsDirectPlay === false;
        const copy = element('span');
        copy.append(element('strong', '', detailVersionTitle(source, index)));
        const meta = detailVersionMeta(source);
        if (meta) copy.append(element('small', '', meta));
        option.append(copy);
        if (selected) option.append(element('span', 'selection-mark', '✓'));
        option.addEventListener('click', onSelect);
        return option;
    }

    function renderSourceOptions(detail) {
        const { options, selected } = detailSourceState(detail);
        const host = byId('source-options');
        host.replaceChildren();
        options.forEach((source, index) => {
            host.append(createSourceOption(source, index, selected?.id === source.id, () => selectDetailSource(detail, source.id)));
        });
    }

    function selectDetailSource(detail, sourceId) {
        const changed = Boolean(detail) && detail.selectedSourceId !== sourceId;
        if (changed) detail.selectedSourceId = sourceId;
        closeSourcePicker();
        if (!changed) return;
        const meta = createDetailMeta(detail);
        if (!replaceDetailField(detail, 'meta', meta)) content.querySelector('.detail-copy')?.append(meta);
        focusElement(content.querySelector('[data-detail-field="meta"] .meta-switch'));
    }

    function openSourcePicker(detail, trigger) {
        sourcePickerTrigger = trigger;
        renderSourceOptions(detail);
        byId('source-scrim').classList.remove('hidden');
        requestAnimationFrame(() => {
            const host = byId('source-options');
            focusElement(host.querySelector('[aria-checked="true"]:not(:disabled)') || host.querySelector('.track-option:not(:disabled)'));
        });
    }

    function closeSourcePicker() {
        if (byId('source-scrim').classList.contains('hidden')) return;
        byId('source-scrim').classList.add('hidden');
        const trigger = sourcePickerTrigger;
        sourcePickerTrigger = null;
        focusElement(trigger);
    }

    function appendDetailPlayAction(actions, target, detail) {
        detailSourceState(detail);
        const play = element('button', 'primary-command');
        play.type = 'button';
        play.dataset.focusKey = `play:${target.id}`;
        const position = target.resumePositionMs ? ` ${formatTime(target.resumePositionMs)}` : '';
        const label = target.type === 'Episode'
            ? `播放 ${episodePosition(target).replace(' · ', '')}${position}`
            : (target.resumePositionMs ? `继续播放${position}` : '开始播放');
        play.append(strokeIcon('play'), element('span', '', label));
        play.addEventListener('click', () => startPlayback(target, target.resumePositionMs, detail?.selectedSourceId || ''));
        actions.prepend(play);
    }

    function openOverview(card, trigger) {
        overviewTrigger = trigger;
        byId('overview-title').textContent = card.title;
        byId('overview-body').textContent = card.overview;
        byId('overview-scrim').classList.remove('hidden');
        requestAnimationFrame(() => focusElement(byId('overview-close')));
    }

    function closeOverview() {
        if (byId('overview-scrim').classList.contains('hidden')) return;
        byId('overview-scrim').classList.add('hidden');
        const trigger = overviewTrigger;
        overviewTrigger = null;
        focusElement(trigger);
    }

    function renderSearch(data = { query: '', draft: '', items: null, status: 'idle', error: '' }) {
        replaceContent();
        if (data.items?.[0]) {
            loadViewBackdrop(data.items[0].backdropImage || data.items[0].landscapeImage, appBackdropRevision, currentView);
        } else if (data.status === 'idle') {
            clearAppBackdrop(appBackdropRevision);
        }
        const root = element('section', 'search-view');
        const form = element('form', 'search-form');
        const back = element('button', 'back-button hero-carousel-previous search-back');
        back.append(element('span', 'hero-carousel-chevron'));
        back.type = 'button';
        back.title = '返回';
        back.setAttribute('aria-label', '返回');
        back.dataset.focusKey = 'search:back';
        back.addEventListener('click', goBack);
        const heading = element('div', 'search-heading');
        heading.append(
            element('h1', '', '搜索媒体库'),
            element('p', '', '按片名、剧集名或关键词查找你的收藏'),
        );
        const header = element('header', 'search-header');
        header.append(back, heading);
        const field = element('div', 'search-field');
        const leading = element('span', 'search-leading');
        leading.setAttribute('aria-hidden', 'true');
        leading.append(element('span', 'search-glyph'));
        field.append(leading);
        const input = document.createElement('input');
        input.type = 'search'; input.placeholder = '搜索电影、剧集'; input.value = data.draft ?? data.query ?? '';
        input.setAttribute('aria-label', '搜索媒体');
        input.dataset.focusKey = 'search:input';
        const clear = element('button', 'search-clear');
        clear.type = 'button';
        clear.title = '清除搜索';
        clear.setAttribute('aria-label', '清除搜索');
        clear.classList.toggle('hidden', !input.value);
        clear.addEventListener('click', () => {
            searchRevision += 1;
            Object.assign(data, { query: '', draft: '', items: null, status: 'idle', error: '' });
            renderSearch(data);
        });
        input.addEventListener('input', () => {
            data.draft = input.value;
            clear.classList.toggle('hidden', !input.value);
            if (data.status === 'loading') {
                searchRevision += 1;
                data.status = 'idle';
                submit.disabled = false;
                root.querySelector('.search-state[role="status"]')?.remove();
            }
        });
        const submit = element('button', 'primary-command', '搜索');
        submit.type = 'submit';
        submit.disabled = data.status === 'loading';
        field.append(input, clear);
        form.append(field, submit);
        form.addEventListener('submit', (event) => {
            event.preventDefault(); void submitSearch(data, input.value);
        });
        root.append(header, form);
        if (data.status === 'loading') {
            const state = element('div', 'search-state', '正在搜索…');
            state.setAttribute('role', 'status');
            root.append(state);
        } else if (data.error) {
            const state = element('div', `search-state${data.status === 'error' ? ' error-state' : ''}`, data.error);
            state.setAttribute('role', 'alert');
            root.append(state);
        } else if (Array.isArray(data.items)) {
            if (data.items.length) {
                const resultsHeading = element('div', 'search-results-heading');
                resultsHeading.append(element('h2', '', `本页 ${data.items.length} 个结果`));
                if (data.query) resultsHeading.append(element('span', '', `“${data.query}”`));
                const grid = element('div', 'grid-view search-grid');
                data.items.forEach((card, index) => grid.append(createMediaCard(card, {
                    index,
                    onClick: openDetail,
                    onFocus: updateBackdropForCard,
                })));
                root.append(resultsHeading, grid);
            } else {
                const empty = element('div', 'search-empty');
                const icon = element('span', 'search-empty-icon');
                icon.append(element('span', 'search-glyph'));
                empty.append(
                    icon,
                    element('strong', '', '没有找到相关内容'),
                    element('p', '', '试试更短的片名、其他译名或不同的关键词。'),
                );
                root.append(empty);
            }
        } else {
            const empty = element('div', 'search-empty');
            const icon = element('span', 'search-empty-icon');
            icon.append(element('span', 'search-glyph'));
            empty.append(
                icon,
                element('strong', '', '找到下一部想看的作品'),
                element('p', '', '输入片名、剧集名或关键词，搜索结果会在这里展示。'),
            );
            root.append(empty);
        }
        content.append(root);
        requestAnimationFrame(() => input.focus());
    }

    async function submitSearch(data, query) {
            if (data.status === 'loading') return;
            query = query.trim();
            if (!query) {
                Object.assign(data, { draft: query, status: 'invalid', error: '请输入搜索内容' });
                renderSearch(data);
                return;
            }
            const revision = ++searchRevision;
            Object.assign(data, { query, draft: query, items: null, status: 'loading', error: '' });
            renderSearch(data);
            try {
                const result = await nativeRequest('mediaStationCatalog', 'search', ['search', JSON.stringify({ query, limit: 60 })]);
                if (revision !== searchRevision || currentView?.kind !== 'search' || currentView.data !== data) return;
                if (!result || !Array.isArray(result.items)) throw new Error('搜索响应缺少结果列表');
                Object.assign(data, { items: result.items, status: 'success', error: '' });
                renderSearch(data);
            } catch (error) {
                if (revision !== searchRevision || currentView?.kind !== 'search' || currentView.data !== data) return;
                Object.assign(data, { items: null, status: 'error', error: `搜索失败：${friendlyError(error)}` });
                renderSearch(data);
            }
    }

    function openSearch() {
        if (currentView?.kind === 'search') {
            focusElement(content.querySelector('.search-field input'));
            return;
        }
        setCurrentView({ kind: 'search', data: { query: '', draft: '', items: null, status: 'idle', error: '' } });
    }

    function validUiTheme(value) {
        return ['light', 'dark'].includes(value);
    }

    function loadUiTheme() {
        try {
            const stored = window.localStorage.getItem(uiThemeSettingKey);
            return validUiTheme(stored) ? stored : (['violet', 'cobalt', 'ember'].includes(stored) ? 'dark' : 'light');
        } catch (error) {
            console.error(`MediaStation UI theme could not be read: ${error}`);
            return 'light';
        }
    }

    function prefersReducedMotion() {
        return document.documentElement.dataset.reduceMotion === 'true'
            || window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    }

    function applyMotionPreference(value, persist = false) {
        document.documentElement.dataset.reduceMotion = String(value);
        byId('settings-reduce-motion').checked = value;
        if (persist) {
            try { window.localStorage.setItem('MediaStationGo.Windows.reduceMotion.v1', String(value)); }
            catch (error) { console.error(error); showToast('动态效果设置保存失败'); }
        }
    }

    function applyUiTheme(theme, persist = false) {
        const selected = validUiTheme(theme) ? theme : 'light';
        document.documentElement.dataset.uiTheme = selected; document.documentElement.style.colorScheme = selected;
        const titlebarColor = getComputedStyle(document.documentElement).getPropertyValue('--bg').trim();
        if (titlebarColor && window.jmpNative?.themeColor) window.jmpNative.themeColor(titlebarColor);
        byId('theme-toggle').title = selected === 'dark' ? '切换为浅色主题' : '切换为深色主题';
        byId('theme-toggle').setAttribute('aria-label', byId('theme-toggle').title);
        document.querySelectorAll('[data-ui-theme]').forEach((button) => {
            button.setAttribute('aria-pressed', String(button.dataset.uiTheme === selected));
        });
        if (!persist) return;
        try {
            window.localStorage.setItem(uiThemeSettingKey, selected);
        } catch (error) {
            console.error(`MediaStation UI theme could not be saved: ${error}`);
            showToast('主题保存失败');
        }
    }

    function setProxySettingsStatus(state, text) {
        proxySettingsStatus = { state, text };
        const status = byId('settings-proxy-status');
        status.textContent = text;
        status.dataset.state = state;
    }

    function normalizeUpdateDownloadSources(raw) {
        const values = [];
        for (const line of String(raw || '').split(/\r?\n/)) {
            const value = line.trim();
            if (!value || values.includes(value)) continue;
            if (value === 'direct') {
                values.push(value);
                continue;
            }
            let url;
            try { url = new URL(value); } catch { return null; }
            if (url.protocol !== 'https:' || url.username || url.password || url.port || url.search || url.hash || !value.endsWith('/')) {
                return null;
            }
            values.push(value);
        }
        return values.length <= 8 ? values.join('\n') : null;
    }

    function refreshUpdateDownloadSourceControls() {
        const input = byId('settings-update-download-sources');
        const save = byId('settings-save-update-download-sources');
        const status = byId('settings-update-download-sources-status');
        const customSources = byId('settings-update-custom-sources');
        if (!input || !save || !status || !customSources) return;
        input.value = updateDownloadSources;
        document.querySelectorAll('input[name="settings-update-source-mode"]').forEach((radio) => {
            radio.checked = radio.value === updateDownloadSourceMode;
        });
        customSources.classList.toggle('hidden', updateDownloadSourceMode !== 'custom');
        input.disabled = false;
        save.disabled = false;
        status.textContent = '失败时会按顺序切换下载源，最后尝试 GitHub 直连。';
        status.dataset.state = '';
    }

    function saveUpdateDownloadSources() {
        const input = byId('settings-update-download-sources');
        const status = byId('settings-update-download-sources-status');
        if (!input || !status) return;
        const normalized = normalizeUpdateDownloadSources(input.value);
        if (!normalized) {
            status.textContent = '下载源格式无效：至少填写一个 HTTPS 前缀或 direct，每行一个，最多 8 个。';
            status.dataset.state = 'error';
            return;
        }
        updateDownloadSources = normalized;
        updateDownloadSourceMode = 'custom';
        window.jmpNative?.setSettingValue?.('advanced', 'updateDownloadSourceMode', updateDownloadSourceMode);
        window.jmpNative?.setSettingValue?.('advanced', 'updateDownloadSources', normalized);
        input.value = normalized;
        status.textContent = '下载源已保存。';
        status.dataset.state = 'ready';
    }

    function resetUpdateDownloadSources() {
        updateDownloadSourceMode = 'server';
        window.jmpNative?.setSettingValue?.('advanced', 'updateDownloadSourceMode', updateDownloadSourceMode);
        refreshUpdateDownloadSourceControls();
        const status = byId('settings-update-download-sources-status');
        if (status) {
            status.textContent = '已恢复默认下载源。';
            status.dataset.state = 'ready';
        }
    }

    function selectSettingsSection(section) {
        const settings = byId('settings-drawer');
        const signedIn = Boolean(session);
        const available = signedIn
            ? ['network', 'appearance', 'playback', 'storage', 'update']
            : ['network'];
        activeSettingsSection = available.includes(section) ? section : 'network';
        settings.classList.toggle('is-signed-out', !signedIn);
        settings.querySelectorAll('.settings-section').forEach((panel) => {
            panel.classList.toggle('hidden', panel.dataset.settingsSection !== activeSettingsSection);
        });
        settings.querySelectorAll('[data-settings-section]').forEach((button) => {
            button.classList.toggle('is-active', button.dataset.settingsSection === activeSettingsSection);
        });
        if (activeSettingsSection === 'update') refreshUpdateDownloadSourceControls();
    }

    function proxyModeInputs() {
        return document.querySelectorAll('input[name="settings-proxy-mode"]');
    }

    function setProxyModeInputs(mode, disabled) {
        proxyModeInputs().forEach((input) => {
            input.checked = input.value === mode;
            input.disabled = disabled;
        });
    }

    function refreshProxySettings() {
        const signedIn = Boolean(session);
        const description = byId('settings-proxy-description');
        setProxyModeInputs(globalProxyMode, false);
        description.textContent = signedIn
            ? '应用于当前会话和之后所有服务器的连接，不会写入账号凭据。'
            : '选择下一次登录媒体服务器时使用的全局连接方式。';
        if (!proxySettingsStatus.text) {
            setProxySettingsStatus(
                '',
                signedIn ? '当前全局连接设置已生效。' : '保存后会在下次登录时生效。',
            );
        } else {
            setProxySettingsStatus(proxySettingsStatus.state, proxySettingsStatus.text);
        }
    }

    async function updateGlobalProxyMode(mode) {
        if (!['direct', 'system'].includes(mode) || mode === globalProxyMode) {
            setProxyModeInputs(globalProxyMode, false);
            return;
        }
        const previous = globalProxyMode;
        setProxyModeInputs(mode, true);
        setProxySettingsStatus('', '正在保存全局连接设置...');
        try {
            const status = await nativeRequest(
                'mediaStationSetProxyMode',
                'set_proxy_mode',
                [mode],
            );
            globalProxyMode = mode;
            if (session && status?.configured) {
                session = { ...session, ...status };
                updateSessionUi();
            }
            setProxySettingsStatus('ready', mode === 'system' ? '已启用 Windows 系统代理。' : '已切换为直连。');
        } catch (error) {
            globalProxyMode = previous;
            setProxyModeInputs(previous, true);
            setProxySettingsStatus('error', friendlyError(error));
        } finally {
            setProxyModeInputs(globalProxyMode, false);
        }
    }

    function settingsMotionDuration(opening) {
        if (prefersReducedMotion()) return 0;
        return opening ? settingsCardOpenMotionDuration : settingsCardCloseMotionDuration;
    }

    function clearSettingsCardMotion() {
        window.cancelAnimationFrame(settingsCardAnimationFrame);
        settingsCardAnimationFrame = 0;
        settingsCardAnimation?.cancel();
        settingsCardAnimation = null;
        settingsMotionLayer?.remove();
        settingsMotionLayer = null;
        byId('settings-drawer').classList.remove('is-animating');
    }

    function animateSettingsCard(trigger, opening, onFinished) {
        clearSettingsCardMotion();
        if (!trigger || !settingsMotionDuration(opening)) {
            onFinished?.();
            return;
        }
        const drawer = byId('settings-drawer');
        const card = drawer.querySelector('.settings-preferences');
        if (!card) return;
        const triggerBounds = trigger.getBoundingClientRect();
        const cardBounds = card.getBoundingClientRect();
        if (!cardBounds.width || !cardBounds.height) return;
        const scaleX = Math.max(triggerBounds.width / cardBounds.width, 0.025);
        const scaleY = Math.max(triggerBounds.height / cardBounds.height, 0.025);
        const translateX = (triggerBounds.left + (triggerBounds.width / 2))
            - (cardBounds.left + (cardBounds.width / 2));
        const translateY = (triggerBounds.top + (triggerBounds.height / 2))
            - (cardBounds.top + (cardBounds.height / 2));
        const collapsedTransform = `translate3d(${translateX}px, ${translateY}px, 0) scale(${scaleX}, ${scaleY})`;
        const triggerRadius = Math.min(triggerBounds.width, triggerBounds.height) / 2;
        const collapsedRadius = `${Math.max(8, triggerRadius / scaleX)}px / ${Math.max(8, triggerRadius / scaleY)}px`;
        const collapsedState = {
            opacity: 0.38,
            transform: collapsedTransform,
            borderRadius: collapsedRadius,
            boxShadow: '0 2px 6px rgba(0,0,0,0.18)',
        };
        const expandedState = {
            opacity: 1,
            transform: 'none',
            borderRadius: '14px',
            boxShadow: '0 28px 58px rgba(0,0,0,0.4)',
        };
        const startState = opening ? collapsedState : expandedState;
        const endState = opening ? expandedState : collapsedState;
        const layer = card.cloneNode(true);
        layer.classList.add('settings-motion-layer');
        layer.setAttribute('aria-hidden', 'true');
        layer.removeAttribute('id');
        layer.querySelectorAll('[id]').forEach((node) => node.removeAttribute('id'));
        // The motion layer shares the document with the live settings card.
        // Keep its copied radios out of the live group, otherwise appending the
        // layer clears the selected proxy mode on the real form.
        layer.querySelectorAll('input[name="settings-proxy-mode"]').forEach((input) => {
            const liveInput = card.querySelector(`input[name="settings-proxy-mode"][value="${input.value}"]`);
            input.name = 'settings-proxy-mode-motion';
            input.checked = liveInput.checked;
        });
        Object.assign(layer.style, {
            left: `${cardBounds.left}px`,
            top: `${cardBounds.top}px`,
            width: `${cardBounds.width}px`,
            height: `${cardBounds.height}px`,
            opacity: String(startState.opacity),
            transform: startState.transform,
            borderRadius: startState.borderRadius,
            boxShadow: startState.boxShadow,
        });
        drawer.classList.add('is-animating');
        document.body.append(layer);
        settingsMotionLayer = layer;
        const animation = layer.animate(
            [startState, endState],
            {
                duration: settingsMotionDuration(opening),
                easing: opening
                    ? 'cubic-bezier(0.16, 0.84, 0.26, 1)'
                    : 'cubic-bezier(0.5, 0, 0.82, 0.2)',
                fill: 'both',
            },
        );
        settingsCardAnimation = animation;
        animation.pause();
        animation.currentTime = 0;
        settingsCardAnimationFrame = requestAnimationFrame(() => {
            settingsCardAnimationFrame = 0;
            if (settingsCardAnimation === animation) animation.play();
        });
        void animation.finished.then(() => {
            if (settingsCardAnimation !== animation) return;
            settingsCardAnimation = null;
            if (settingsMotionLayer === layer) settingsMotionLayer = null;
            if (!opening) drawer.classList.add('is-closed');
            layer.remove();
            drawer.classList.remove('is-animating');
            if (opening && currentDrawer === drawer) {
                focusElement(drawer.querySelector('.settings-nav .is-active, .settings-preferences .drawer-close'));
            }
            onFinished?.();
        }).catch(() => {});
    }

    const videoComponentStatuses = new Map();
    let videoComponentsTimer = 0;
    let videoComponentsTracking = false;
    let videoComponentsRestarting = false;
    function updateVideoComponents(payload) {
        if (!Array.isArray(payload?.components)) throw new Error('组件状态列表缺失');
        let busy = false;
        for (const status of payload.components) {
            if (!['rtx-video', 'dlssnr', 'nvidia-vfg'].includes(status.id)) throw new Error('组件身份无效');
            videoComponentStatuses.set(status.id, status);
            const card = document.querySelector(`[data-component-id="${status.id}"]`);
            const label = byId(`component-${status.id}-status`);
            const blocked = videoComponentsRestarting || status.restarting || status.busy || status.supported === false || typeof status.installed !== 'boolean';
            card.querySelectorAll('[data-component-operation]').forEach((button) => {
                const op = button.dataset.componentOperation;
                button.disabled = Boolean(blocked || (['install', 'import'].includes(op) && (status.installed || status.restartRequired))
                    || (op === 'uninstall' && (!status.installed || status.restartRequired))
                    || (op === 'cancel' && status.pendingOperation !== 'install')
                    || (op === 'restart' && (status.pendingOperation !== 'install' || status.error || status.runtime?.failureCode === 'frame_interpolation_component_invalid')));
            });
            card.querySelector('.component-pending').classList.toggle('hidden', status.pendingOperation !== 'install');
            const runtime = status.runtime || {};
            label.dataset.state = status.error || runtime.failureCode ? 'error' : status.busy || status.restartRequired ? 'pending' : 'ready';
            label.textContent = status.busy ? (status.progress?.phase === 'selecting' ? '请选择本地组件包...' : status.progress?.phase === 'downloading'
                ? `正在下载：${formatBytes(status.progress.downloadedBytes)} / ${formatBytes(status.progress.totalBytes)}` : '正在校验组件包...')
                : status.error ? status.error.message : status.restartRequired ? (runtime.failureCode ? `启动时未能执行组件变更：${runtime.failureDetail}；待操作已保留。` : '组件变更已排队，退出所有实例并重新启动后生效。')
                : runtime.failureCode ? runtime.failureDetail : !status.installed ? '未安装，可按需安装。'
                : runtime.experimental ? `已安装运行库。${runtime.failureDetail}` : runtime.playbackAvailable ? '已安装；播放时验证 GPU 支持并运行原生处理。' : '已安装，原生播放桥不可用。';
            busy ||= Boolean(status.busy);
        }
        videoComponentsTracking = busy;
        window.clearTimeout(videoComponentsTimer); videoComponentsTimer = 0;
        if (busy) scheduleVideoComponentsRefresh();
        refreshVideoEnhancementEntry();
    }
    function scheduleVideoComponentsRefresh(delay = 1500) {
        window.clearTimeout(videoComponentsTimer);
        videoComponentsTimer = window.setTimeout(() => { videoComponentsTimer = 0; refreshVideoComponents(); }, delay);
    }
    async function refreshVideoComponents() {
        try { updateVideoComponents(await nativeRequest('mediaStationFrameInterpolation', 'video_components_status', ['video_components_status'], 15000)); }
        catch (error) {
            for (const id of ['rtx-video', 'dlssnr', 'nvidia-vfg']) { const label = byId(`component-${id}-status`); label.textContent = `状态查询失败：${friendlyError(error)}`; label.dataset.state = 'error'; }
            if (videoComponentsTracking) scheduleVideoComponentsRefresh(3000);
        }
    }
    async function changeVideoComponent(id, operation) {
        if (videoComponentsRestarting) return;
        const label = byId(`component-${id}-status`);
        try {
            if (operation === 'restart') videoComponentsRestarting = true;
            videoComponentsTracking = true;
            const op = `video_component_${operation}`;
            const result = await nativeRequest('mediaStationFrameInterpolation', op, [op, id], 15000);
            if (operation === 'restart') {
                if (result?.restarting !== true) throw new Error('重启助手未确认就绪');
                label.textContent = '正在退出并重新启动播放器...';
            } else updateVideoComponents(result);
        } catch (error) {
            videoComponentsRestarting = false; await refreshVideoComponents();
            label.textContent = `组件操作失败：${friendlyError(error)}`; label.dataset.state = 'error';
        }
    }
    function refreshVideoEnhancementEntry() {
        const entry = byId('player-interpolation');
        const installed = entry.dataset.componentInstalled === 'true' || [...videoComponentStatuses.values()].some((status) => status.installed === true && !status.runtime?.experimental);
        entry.classList.toggle('hidden', !installed);
        const ready = entry.dataset.componentReady === 'true' || [...videoComponentStatuses.values()].some((status) => status.runtime?.playbackAvailable === true);
        entry.dataset.enhancementReady = String(ready);
        refreshPlayerTools();
    }

    function openSettings(trigger) {
        selectSettingsSection(activeSettingsSection);
        refreshProxySettings();
        if (session) {
            if (currentView?.kind !== 'settings') setCurrentView({ kind: 'settings', data: {} });
            refreshFrameInterpolationStatus(); refreshVideoComponents(); refreshUpdateControls();
            return;
        }
        openDrawer(byId('settings-drawer'), trigger);
        animateSettingsCard(trigger, true);
        if (session) {
            refreshFrameInterpolationStatus();
            refreshUpdateControls();
        }
    }

    function renderComponents() {
        replaceContent();
        const page = byId('components-page'); page.classList.remove('hidden'); content.append(page);
        refreshFrameInterpolationStatus(); refreshVideoComponents();
    }

    function renderSettings() {
        replaceContent();
        const settings = byId('settings-drawer');
        settings.classList.add('settings-page', 'open'); settings.classList.remove('is-closed', 'is-closing', 'is-animating');
        settings.removeAttribute('aria-modal'); settings.setAttribute('role', 'region'); settings.setAttribute('aria-hidden', 'false');
        content.append(settings); selectSettingsSection(activeSettingsSection); refreshProxySettings();
    }

    function positionAccountPopover(drawer = byId('account-drawer'), trigger = byId('account-open')) {
        if (!drawer || !trigger?.getBoundingClientRect) return;
        const rect = trigger.getBoundingClientRect();
        const edge = 14;
        const panelWidth = Math.min(430, Math.max(0, window.innerWidth - edge * 2));
        const left = clamp(rect.left, edge, Math.max(edge, window.innerWidth - panelWidth - edge));
        const bottom = Math.max(edge, window.innerHeight - rect.top + 8);
        drawer.style.setProperty('--account-popover-left', `${Math.round(left)}px`);
        drawer.style.setProperty('--account-popover-bottom', `${Math.round(bottom)}px`);
    }

    function openDrawer(drawer, trigger) {
        closeDrawer(false);
        const settingsDrawer = byId('settings-drawer');
        if (drawer === settingsDrawer || settingsDrawer.classList.contains('is-closing')) {
            clearSettingsCardMotion();
            settingsDrawer.classList.remove('is-closed', 'is-closing');
        }
        drawer.classList.remove('is-closed', 'is-closing');
        currentDrawer = drawer;
        drawerTrigger = trigger;
        if (drawer === byId('account-drawer')) positionAccountPopover(drawer, trigger);
        window.clearTimeout(drawerScrimCloseTimer);
        const scrim = byId('drawer-scrim');
        scrim.classList.remove('hidden', 'is-closing', 'is-settings-closing');
        drawer.classList.add('open');
        drawer.setAttribute('aria-hidden', 'false');
        requestAnimationFrame(() => {
            if (drawer === byId('settings-drawer') && drawer.classList.contains('is-animating')) return;
            const target = drawer === byId('settings-drawer')
                ? drawer.querySelector('.settings-nav .is-active, .settings-preferences .drawer-close')
                : drawer.querySelector('button, input');
            focusElement(target);
        });
    }

    function closeDrawer(restoreFocus = true) {
        if (!currentDrawer) return;
        const closingSettings = currentDrawer === byId('settings-drawer');
        const closingDrawer = currentDrawer;
        const trigger = drawerTrigger;
        if (!closingSettings) savedAccountsRevision += 1;
        closingDrawer.classList.toggle('is-closing', closingSettings);
        closingDrawer.classList.remove('open');
        closingDrawer.setAttribute('aria-hidden', 'true');
        const scrim = byId('drawer-scrim');
        scrim.classList.add('is-closing');
        scrim.classList.toggle('is-settings-closing', closingSettings);
        window.clearTimeout(drawerScrimCloseTimer);
        currentDrawer = null;
        drawerTrigger = null;
        if (closingSettings) {
            animateSettingsCard(trigger, false, () => {
                if (currentDrawer) return;
                scrim.classList.add('hidden');
                scrim.classList.remove('is-closing', 'is-settings-closing');
                closingDrawer.classList.remove('is-closing');
            });
        } else {
            drawerScrimCloseTimer = window.setTimeout(() => {
                if (currentDrawer) return;
                scrim.classList.add('hidden');
                scrim.classList.remove('is-closing', 'is-settings-closing');
            }, 230);
        }
        if (restoreFocus) trigger?.focus();
    }

    function isCurrentAccount(account) {
        return Boolean(session?.accountId && account?.accountId === session.accountId);
    }

    function isCurrentServer(server) {
        return Boolean(session?.serverId && server?.serverId === session.serverId);
    }

    function serverLabel(baseUrl) {
        try {
            const url = new URL(baseUrl);
            return url.host;
        } catch {
            return baseUrl;
        }
    }

    function returnToAccountDrawer() {
        loginCanCancel = false;
        loginMode = 'add';
        loginAccount = null;
        loginReturnFocus = null;
        loginView.classList.add('hidden');
        appShell.classList.remove('hidden');
        byId('server-url').readOnly = false;
        setLoginConnection(null, false);
        byId('login-cancel').classList.add('hidden');
        openDrawer(byId('account-drawer'), byId('account-open'));
        loadSavedAccounts();
    }

    function accountListElements(surface) {
        return surface === 'login'
            ? { list: byId('login-accounts-list'), state: byId('login-accounts-state') }
            : { list: byId('saved-accounts-list'), state: byId('saved-accounts-state') };
    }

    async function readSavedAccountServers() {
        const result = await nativeRequest('mediaStationListAccounts', 'list_accounts');
        const servers = Array.isArray(result.servers) ? result.servers : [];
        const accountIds = new Set();
        if (servers.some((server) => (
            typeof server.serverId !== 'string'
            || !/^[0-9a-f]{64}$/.test(server.serverId)
            || typeof server.baseUrl !== 'string'
            || !server.baseUrl
            || !validConnectionProfile(server)
            || !Array.isArray(server.users)
            || server.users.some((account) => (
                typeof account.accountId !== 'string'
                || !/^[0-9a-f]{64}$/.test(account.accountId)
                || typeof account.userId !== 'string'
                || !account.userId
                || typeof account.userName !== 'string'
            ))
        ))) {
            throw new Error('已保存账号数据无效');
        }
        servers.forEach((server) => server.users.forEach((account) => {
            if (accountIds.has(account.accountId)) throw new Error('已保存账号数据重复');
            accountIds.add(account.accountId);
        }));
        return servers;
    }

    function accountFromSavedServer(server, user) {
        return {
            ...user,
            baseUrl: server.baseUrl,
            serverId: server.serverId,
            clientProfile: server.clientProfile,
        };
    }

    function embyClientIdentityLabel(profile) {
        if (profile === 'senplayer') return 'SenPlayer';
        if (profile === 'infuse') return 'Infuse';
        return '默认身份';
    }

    function renderSavedAccountServers(servers, surface) {
        const { list, state } = accountListElements(surface);
        const accounts = servers.flatMap((server) => server.users);
        if (!accounts.length) {
            state.textContent = '暂无已保存账号';
            state.classList.remove('hidden');
            list.replaceChildren();
            return false;
        }
        const orderedServers = servers.slice().sort((left, right) => {
            const leftActive = isCurrentServer(left);
            const rightActive = isCurrentServer(right);
            if (leftActive !== rightActive) return leftActive ? -1 : 1;
            return left.baseUrl.localeCompare(right.baseUrl);
        });
        list.replaceChildren();
        state.classList.add('hidden');
        let animationIndex = 0;
        orderedServers.forEach((server) => {
            const serverSection = element('section', 'saved-account-server', null);
            const serverHeader = element('header', 'saved-account-server-header', null);
            const serverCopy = element('div', 'saved-account-server-copy', null);
            serverCopy.append(element('strong', '', serverLabel(server.baseUrl)));
            serverCopy.append(element('span', 'saved-account-server-connection', server.baseUrl));
            serverCopy.append(element(
                'span',
                'saved-account-server-client',
                `Emby · ${embyClientIdentityLabel(server.clientProfile)}`,
            ));
            const serverCount = element('span', 'saved-account-server-count', `${server.users.length} 个用户`);
            const addUser = element('button', 'icon-button saved-account-server-add', '\u002b');
            addUser.type = 'button';
            addUser.title = '在此服务器添加用户';
            addUser.setAttribute('aria-label', '在此服务器添加用户');
            addUser.addEventListener('click', () => beginAddAccount(server.baseUrl, true, addUser, server));
            serverHeader.append(serverCopy, serverCount, addUser);
            const users = element('div', 'saved-account-users', null);
            const sortedUsers = server.users.slice().sort((left, right) => {
                const leftActive = isCurrentAccount(accountFromSavedServer(server, left));
                const rightActive = isCurrentAccount(accountFromSavedServer(server, right));
                if (leftActive !== rightActive) return leftActive ? -1 : 1;
                return String(left.userName || left.userId).localeCompare(
                    String(right.userName || right.userId),
                    'zh-CN',
                );
            });
            sortedUsers.forEach((user) => {
                const account = accountFromSavedServer(server, user);
                const active = isCurrentAccount(account);
                const row = element('div', 'saved-account-row' + (active ? ' active' : ''), null);
                const switchButton = element('button', 'saved-account-switch', null);
                switchButton.type = 'button';
                if (active) switchButton.setAttribute('aria-current', 'true');
                const avatar = element('span', 'avatar', initials(user.userName || user.userId));
                const body = element('div', 'saved-account-copy', null);
                body.append(element('strong', '', user.userName || user.userId));
                switchButton.append(avatar, body);
                if (active) switchButton.append(element('span', 'saved-account-current', '当前'));
                switchButton.addEventListener('click', () => switchAccount(account, active, switchButton, surface));
                const actions = element('div', 'saved-account-actions', null);
                const edit = element('button', 'icon-button', '\u270e');
                edit.type = 'button';
                edit.title = '修改账号';
                edit.setAttribute('aria-label', `修改 ${user.userName || user.userId}`);
                edit.addEventListener('click', () => beginUpdateAccount(account, edit));
                const remove = element('button', 'icon-button account-delete-action', '\u{1f5d1}\ufe0e');
                remove.type = 'button';
                remove.title = '删除账号';
                remove.setAttribute('aria-label', `删除 ${user.userName || user.userId}`);
                remove.addEventListener('click', () => openDeleteAccountDialog(account, remove, surface));
                actions.append(edit, remove);
                row.append(switchButton, actions);
                row.style.animationDelay = `${Math.min(animationIndex * 30, 240)}ms`;
                animationIndex += 1;
                users.append(row);
            });
            serverSection.append(serverHeader, users);
            list.append(serverSection);
        });
        return true;
    }

    async function loadSavedAccounts(surface = 'drawer', baseUrl = '', connection = null) {
        const { list, state } = accountListElements(surface);
        const revision = ++savedAccountsRevision;
        list.replaceChildren();
        state.textContent = '正在读取账号...';
        state.classList.remove('hidden', 'error');
        try {
            const servers = await readSavedAccountServers();
            if (revision !== savedAccountsRevision) return;
            const hasAccounts = renderSavedAccountServers(servers, surface);
            if (!hasAccounts && surface === 'login') showLogin(baseUrl, '', false, 'login', null, connection);
            if (surface === 'login') {
                requestAnimationFrame(() => focusElement(
                    list.querySelector('.saved-account-switch') || byId('login-add-account'),
                ));
            }
        } catch (error) {
            if (revision !== savedAccountsRevision) return;
            const message = friendlyError(error);
            state.textContent = `账号列表加载失败：${message}`;
            state.classList.add('error');
            state.classList.remove('hidden');
            console.error(`账号列表加载失败：${message}`);
        }
    }

    async function switchAccount(account, alreadyActive, selectedButton, surface = 'drawer') {
        if (alreadyActive) {
            if (surface === 'drawer') closeDrawer();
            return;
        }
        const { list, state } = accountListElements(surface);
        list.querySelectorAll('button').forEach((button) => { button.disabled = true; });
        selectedButton?.closest('.saved-account-row')?.classList.add('busy');
        selectedButton?.setAttribute('aria-busy', 'true');
        const spinner = element('span', 'saved-account-spinner', '');
        selectedButton?.append(spinner);
        // Keep the switching animation visible for at least this long even if
        // the native switch completes instantly, so the transition reads as
        // deliberate instead of flashing.
        const minimumVisible = new Promise((resolve) => window.setTimeout(resolve, 700));
        try {
            const [status] = await Promise.all([
                nativeRequest('mediaStationSwitchAccount', 'switch_account', [account.accountId]),
                minimumVisible,
            ]);
            if (surface === 'drawer') closeDrawer(false);
            resetCatalogState();
            await showApp(status);
        } catch (error) {
            const message = friendlyError(error);
            state.textContent = `账号切换失败：${message}`;
            state.classList.add('error');
            showToast(message);
            list.querySelectorAll('button').forEach((button) => { button.disabled = false; });
            selectedButton?.closest('.saved-account-row')?.classList.remove('busy');
            spinner.remove();
        } finally {
            selectedButton?.removeAttribute('aria-busy');
        }
    }

    function openDeleteAccountDialog(account, trigger, surface = 'drawer') {
        pendingDeleteAccount = account;
        deleteReturnFocus = trigger;
        deleteReturnSurface = surface;
        const active = isCurrentAccount(account);
        byId('account-delete-message').textContent = active
            ? `确定删除“${account.userName || account.userId}”？删除当前账号后将退出当前会话。`
            : `确定删除“${account.userName || account.userId}”？该账号的保存凭据将从此设备移除。`;
        byId('account-delete-scrim').classList.remove('hidden');
        window.setTimeout(() => byId('account-delete-confirm').focus(), 0);
    }

    function closeDeleteAccountDialog(restoreFocus = true) {
        byId('account-delete-scrim').classList.add('hidden');
        const trigger = deleteReturnFocus;
        pendingDeleteAccount = null;
        deleteReturnFocus = null;
        deleteReturnSurface = 'drawer';
        if (restoreFocus) focusElement(trigger || byId('account-open'));
    }

    async function confirmDeleteAccount() {
        const account = pendingDeleteAccount;
        if (!account) return;
        const confirm = byId('account-delete-confirm');
        const cancel = byId('account-delete-cancel');
        confirm.disabled = true;
        cancel.disabled = true;
        const active = isCurrentAccount(account);
        const surface = deleteReturnSurface;
        try {
            await nativeRequest('mediaStationDeleteAccount', 'delete_account', [account.accountId]);
            closeDeleteAccountDialog(false);
            if (active) {
                closeDrawer(false);
                session = null;
                resetCatalogState();
                await showAccountGateway(account.baseUrl, account, '账号已删除');
            } else {
                await loadSavedAccounts(surface, account.baseUrl, account);
                showToast('账号已删除');
            }
        } catch (error) {
            showToast(friendlyError(error));
        } finally {
            confirm.disabled = false;
            cancel.disabled = false;
        }
    }

    function playbackPreferenceScope(card) {
        return card?.type === 'Episode' && card.seriesId ? card.seriesId : card?.id || '';
    }

    function cachedSeriesEpisodes(card) {
        // Detail pages cache one season at a time. The player needs a complete
        // series for cross-season switching and automatic continuation.
        return null;
    }

    async function startPlayback(card, startMs = 0, mediaSourceId = '') {
        dismissPlaybackFailure();
        if (!card?.id || !card.playable) {
            showToast('该项目不能直接播放');
            return;
        }
        const seriesPlaybackSetting = card.type === 'Episode' && card.seriesId
            ? seriesPlaybackSettingFor(card.seriesId)
            : { ...seriesPlaybackSettingDefaults };
        const activePlayer = {
            card,
            mediaSourceId,
            resumeCardRects: currentView?.kind === 'home'
                ? captureMediaRowRects('resume')
                : new Map(),
            playbackSettingsScopeId: playbackPreferenceScope(card),
            playing: true,
            started: false,
            buffering: false,
            exiting: false,
            stopSent: false,
            exitArmed: false,
            positionMs: startMs || 0,
            durationMs: card.durationMs || 0,
            loadInfo: null,
            panelKind: '',
            trackChanging: false,
            trackRefreshing: false,
            metadataRefreshMediaId: null,
            scrubbing: false,
            scrubPositionMs: null,
            interpolationEnabled: false,
            interpolationModel: null,
            interpolationChanging: false,
            interpolationTargetModel: null,
            interpolationResumePlaying: true,
            episodes: cachedSeriesEpisodes(card),
            episodesLoading: false,
            episodesLoadPromise: null,
            episodesError: '',
            episodeSeason: Number.isFinite(card.parentIndexNumber) ? card.parentIndexNumber : 0,
            episodeChanging: false,
            episodeTarget: null,
            episodeResumePlaying: true,
            seriesPlaybackSetting,
            introSkipHandled: seriesPlaybackSetting.introSkipSeconds === 0
                || Math.max(0, Math.round(startMs || 0)) >= seriesPlaybackSetting.introSkipSeconds * 1000,
            outroSkipHandled: false,
            autoAdvancePending: false,
            autoAdvanceResumePlaying: true,
            playbackFinishedDuringAdvance: false,
            automationDisabled: false,
            automationWarningShown: false,
        };
        const playbackSettings = playerSettingsForScope(activePlayer.playbackSettingsScopeId);
        playerVolume = playbackSettings.volume;
        lastAudiblePlayerVolume = playbackSettings.lastAudibleVolume;
        playerPlaybackRate = 1;
        playerSubtitleStyleCustomized = playbackSettings.subtitleStyle !== null;
        playerSubtitleStyle = playbackSettings.subtitleStyle
            ? { ...playbackSettings.subtitleStyle }
            : { ...defaultPlayerSubtitleStyle };
        player = activePlayer;
        danmakuRenderer.clear();
        pushDanmakuEnabled();
        pushDanmakuStyle();
        byId('player-title').textContent = cardTitle(card, true);
        byId('player-subtitle').textContent = card.type === 'Episode' ? [episodePosition(card), card.title].filter(Boolean).join(' · ') : cardSubtitle(card);
        byId('player-loading-title').textContent = cardTitle(card, true);
        byId('player-loading-meta').textContent = [card.type === 'Episode' ? episodePosition(card) : card.year,
            formatDuration(card.durationMs), card.dynamicRange,
            startMs > 0 ? '从 ' + formatTime(startMs) + ' 继续' : ''].filter(Boolean).join(' · ');
        closePlayerMore(false);
        setPlayerPoster(card);
        setPlayerLoading(true, '正在准备播放');
        updatePlayerProgress();
        playerPanel.classList.add('hidden');
        playerPanelTrigger = null;
        refreshPlayerTools();
        hidePlayerFeedback();
        playerView.classList.remove('hidden');
        heroCarouselController?.pause('player');
        setPlayerMode(true);
        applyPlayerVolume(playerVolume, false);
        applyPlayerPlaybackRate(playerPlaybackRate);
        applyPlayerSubtitleStyle(playerSubtitleStyle, false);
        showPlayerControls();
        try {
            await waitForPlayerPaint();
            const loadInfo = await nativeRequest(
                'mediaStationLoad',
                'load',
                [
                    card.id,
                    Math.max(0, Math.round(startMs || 0)),
                    'off',
                    preferredInterpolationModel,
                    playbackPreferenceScope(card),
                    playerSubtitleStyle.fontSize,
                    playerSubtitlePosition(),
                    mediaSourceId || '',
                ],
                60000,
            );
            if (player === activePlayer) {
                activePlayer.loadInfo = loadInfo;
                schedulePlaybackMetadataRefresh(activePlayer);
                if (activePlayer.exiting) return;
                applyPlayerPlaybackRate(1);
                refreshPlayerTools();
                if (activePlayer.card.type === 'Episode' && !Array.isArray(activePlayer.episodes)) {
                    void loadPlayerEpisodes(activePlayer);
                }
            }
        } catch (error) {
            if (player !== activePlayer) return;
            if (activePlayer.exiting && activePlayer.stopSent) return;
            if (activePlayer.exiting) finishPlayer();
            else showPlaybackFailure(friendlyError(error));
        }
    }

    function applyPlaybackMetadataRefresh(activePlayer, tracks) {
        if (!activePlayer?.loadInfo || !tracks) return;
        activePlayer.loadInfo.audioTracks = Array.isArray(tracks.audioTracks) ? tracks.audioTracks : [];
        activePlayer.loadInfo.subtitleTracks = Array.isArray(tracks.subtitleTracks) ? tracks.subtitleTracks : [];
        activePlayer.loadInfo.audioTrackKey = tracks.audioTrackKey || null;
        activePlayer.loadInfo.subtitleTrackKey = tracks.subtitleTrackKey || null;
        activePlayer.loadInfo.subtitleEnabled = tracks.subtitleEnabled === true;
        activePlayer.loadInfo.sourceVideo = tracks.sourceVideo || null;
        activePlayer.loadInfo.container = tracks.container || null;
        activePlayer.loadInfo.bitrate = Number.isFinite(tracks.bitrate) ? tracks.bitrate : null;
        activePlayer.loadInfo.mediaMetadataPending = tracks.mediaMetadataPending === true;
    }

    function schedulePlaybackMetadataRefresh(activePlayer) {
        if (player !== activePlayer
            || !activePlayer.started
            || !activePlayer.loadInfo
            || activePlayer.exiting) return;
        const mediaId = activePlayer.card?.id;
        if (!mediaId || activePlayer.metadataRefreshMediaId === mediaId) return;
        activePlayer.metadataRefreshMediaId = mediaId;
        void refreshPlaybackMetadata(activePlayer, mediaId);
    }

    async function refreshPlaybackMetadata(activePlayer, mediaId) {
        const retryDelaysMs = [0, 1000, 2000, 4000];
        for (const retryDelayMs of retryDelaysMs) {
            if (retryDelayMs > 0) {
                await new Promise((resolve) => window.setTimeout(resolve, retryDelayMs));
            }
            if (player !== activePlayer
                || activePlayer.exiting
                || activePlayer.card?.id !== mediaId) return;
            if (retryDelayMs > 0 && !activePlayer.loadInfo?.mediaMetadataPending) return;
            try {
                const tracks = await nativeRequest(
                    'mediaStationTracks',
                    'tracks',
                    [mediaId],
                    15000,
                );
                if (player !== activePlayer || activePlayer.exiting || activePlayer.card?.id !== mediaId) return;
                applyPlaybackMetadataRefresh(activePlayer, tracks);
                if (tracks.metadataRefreshErrorCode) {
                    console.error(`Playback metadata refresh failed: ${tracks.metadataRefreshErrorCode}`);
                }
                refreshPlayerTools();
                if (activePlayer.panelKind === 'info') renderPlayerPanel();
                if (!activePlayer.loadInfo.mediaMetadataPending) {
                    return;
                }
            } catch (error) {
                console.error(`Playback metadata refresh request failed: ${friendlyError(error)}`);
            }
        }
        if (player === activePlayer
            && !activePlayer.exiting
            && activePlayer.card?.id === mediaId
            && activePlayer.loadInfo?.mediaMetadataPending) {
            console.warn(`Server playback metadata remained pending after bounded refresh: media_id=${mediaId}`);
        }
    }

    function orderedPlayableEpisodes(activePlayer) {
        if (!Array.isArray(activePlayer?.episodes)) return [];
        return activePlayer.episodes
            .filter((episode) => episode?.playable)
            .slice()
            .sort((left, right) => {
                const season = episodeSeasonNumber(left) - episodeSeasonNumber(right);
                if (season !== 0) return season;
                const index = (left.indexNumber || 0) - (right.indexNumber || 0);
                if (index !== 0) return index;
                return String(left.title || '').localeCompare(String(right.title || ''), 'zh-CN');
            });
    }

    function nextPlayerEpisode(activePlayer) {
        const episodes = orderedPlayableEpisodes(activePlayer);
        const current = episodes.findIndex((episode) => episode.id === activePlayer?.card?.id);
        return current >= 0 ? episodes[current + 1] || null : null;
    }

    function validateEpisodeAutomationBounds(activePlayer) {
        if (activePlayer.automationDisabled || !activePlayer.durationMs) return !activePlayer.automationDisabled;
        const introEndMs = activePlayer.seriesPlaybackSetting.introSkipSeconds * 1000;
        const outroStartMs = activePlayer.durationMs - activePlayer.seriesPlaybackSetting.outroSkipSeconds * 1000;
        const invalidIntro = introEndMs > 0 && introEndMs >= activePlayer.durationMs;
        const invalidOutro = activePlayer.seriesPlaybackSetting.outroSkipSeconds > 0 && outroStartMs <= 0;
        const overlaps = introEndMs > 0
            && activePlayer.seriesPlaybackSetting.outroSkipSeconds > 0
            && introEndMs >= outroStartMs;
        if (!invalidIntro && !invalidOutro && !overlaps) return true;
        activePlayer.automationDisabled = true;
        if (!activePlayer.automationWarningShown) {
            activePlayer.automationWarningShown = true;
            showToast('本集时长与片头片尾配置冲突，已停止本集自动跳过');
        }
        return false;
    }

    function maybeHandleEpisodeAutomation(activePlayer, event) {
        if (player !== activePlayer
            || activePlayer.card.type !== 'Episode'
            || activePlayer.exiting
            || activePlayer.episodeChanging
            || activePlayer.autoAdvancePending
            || !activePlayer.started
            || event.seeking === true
            || !validateEpisodeAutomationBounds(activePlayer)) return;

        const introEndMs = activePlayer.seriesPlaybackSetting.introSkipSeconds * 1000;
        if (!activePlayer.introSkipHandled && introEndMs > 0) {
            if (activePlayer.positionMs >= introEndMs) {
                activePlayer.introSkipHandled = true;
            } else if (window.jmpNative) {
                activePlayer.introSkipHandled = true;
                try {
                    activePlayer.positionMs = introEndMs;
                    updatePlayerProgress();
                    window.jmpNative.playerSeek(introEndMs);
                    showPlayerFeedback('已跳过片头');
                } catch (error) {
                    activePlayer.introSkipHandled = false;
                    console.error(`MediaStation intro skip failed: ${error}`);
                    showToast('片头跳过失败');
                }
                return;
            }
        }

        const setting = activePlayer.seriesPlaybackSetting;
        if (!setting.autoNext || setting.outroSkipSeconds === 0 || activePlayer.outroSkipHandled) return;
        const outroStartMs = activePlayer.durationMs - setting.outroSkipSeconds * 1000;
        if (activePlayer.positionMs < outroStartMs) return;
        activePlayer.outroSkipHandled = true;
        void advanceToNextEpisode(activePlayer, 'outro');
    }

    async function advanceToNextEpisode(activePlayer, trigger) {
        if (player !== activePlayer
            || activePlayer.exiting
            || activePlayer.episodeChanging
            || activePlayer.autoAdvancePending
            || activePlayer.seriesPlaybackSetting.autoNext !== true) return false;

        activePlayer.autoAdvancePending = true;
        activePlayer.autoAdvanceResumePlaying = activePlayer.playing;
        if (trigger === 'finished') activePlayer.playbackFinishedDuringAdvance = true;
        closePlayerPanel(false);
        setPlayerLoading(true, trigger === 'outro' ? '正在跳过片尾' : '正在准备下一集');
        refreshPlayerTools();

        const episodes = await loadPlayerEpisodes(activePlayer);
        if (player !== activePlayer || activePlayer.exiting) return false;
        const nextEpisode = episodes.length ? nextPlayerEpisode(activePlayer) : null;
        if (!nextEpisode) {
            activePlayer.autoAdvancePending = false;
            refreshPlayerTools();
            if (trigger === 'finished' || activePlayer.playbackFinishedDuringAdvance) {
                finishPlayer();
            } else {
                setPlayerLoading(activePlayer.buffering, activePlayer.buffering ? '正在缓冲' : '');
                if (activePlayer.autoAdvanceResumePlaying && window.jmpNative) window.jmpNative.playerPlay();
            }
            return false;
        }

        activePlayer.autoAdvancePending = false;
        return switchPlayerEpisode(nextEpisode, { automatic: true, trigger });
    }

    function handlePlaybackEvent(event) {
        // The stopped-session report has landed server-side. Verify the local
        // Continue Watching update after the player has returned to the app;
        // the signal may race the terminal playback event.
        if (event.kind === 'home_stale') {
            if (player) {
                homeVerificationPending = true;
            } else {
                homeVerificationPending = false;
                scheduleHomeRefresh(200);
            }
            return;
        }
        if (!player) return;
        if (event.kind === 'canceled' && (player.interpolationChanging || player.episodeChanging) && !player.exiting) return;
        // Only position-bearing events may change the player timeline. Other
        // event snapshots can still contain the load-time or terminal zero.
        if (['position', 'seeked'].includes(event.kind)
            && Number.isFinite(event.positionMs)
            && !player.scrubbing) {
            player.positionMs = event.positionMs;
        }
        if (Number.isFinite(event.durationMs) && event.durationMs > 0) player.durationMs = event.durationMs;
        updatePlayerProgress();
        danmakuRenderer.updatePlayback(event, event.kind === 'paused' ? false : player.playing);
        if (event.kind === 'position') maybeHandleEpisodeAutomation(player, event);
        if (player.exiting && !player.stopSent && event.kind === 'started') {
            sendPlayerStop(player);
        }
        if (!player || (player.exiting && !['finished', 'canceled', 'error'].includes(event.kind))) return;
        switch (event.kind) {
            case 'started':
                {
                    const firstFrame = !player.started;
                    const interpolationChanged = player.interpolationChanging;
                    const episodeChanged = player.episodeChanging;
                    const shouldPlay = interpolationChanged
                        ? player.interpolationResumePlaying
                        : (episodeChanged ? player.episodeResumePlaying : true);
                    if (interpolationChanged) {
                        player.interpolationEnabled = player.interpolationTargetModel !== null;
                        player.interpolationModel = player.interpolationTargetModel;
                        player.interpolationChanging = false;
                        player.interpolationTargetModel = null;
                    }
                    if (episodeChanged) {
                        player.episodeChanging = false;
                        player.episodeTarget = null;
                    }
                    player.started = true;
                    player.playing = shouldPlay;
                    player.buffering = false;
                    if (!shouldPlay && window.jmpNative) window.jmpNative.playerPause();
                    refreshPlayerTools();
                    setPlayerLoading(false);
                    schedulePlaybackMetadataRefresh(player);
                    if (firstFrame && !player.panelKind) {
                        hidePlayerFeedback();
                        hidePlayerControls();
                    } else {
                        showPlayerControls();
                    }
                }
                break;
            case 'paused':
                if (player.interpolationChanging || player.episodeChanging || player.autoAdvancePending) {
                    player.playing = player.interpolationChanging
                        ? player.interpolationResumePlaying
                        : (player.episodeChanging ? player.episodeResumePlaying : player.autoAdvanceResumePlaying);
                    refreshPlayerTools();
                    showPlayerControls();
                    break;
                }
                player.playing = false;
                refreshPlayerTools();
                if (!player.started) {
                    hidePlayerFeedback();
                    break;
                }
                showPlayerControls();
                break;
            case 'buffering':
                player.buffering = event.buffering === true;
                if (player.interpolationChanging || player.episodeChanging) {
                    setPlayerLoading(true, playerTransitionLoadingLabel(player));
                } else if (player.buffering) {
                    setPlayerLoading(
                        true,
                        player.started ? '正在缓冲' : '正在准备播放',
                    );
                } else {
                    setPlayerLoading(!player.started, '正在准备播放');
                }
                break;
            case 'finished':
                if (player.exiting) {
                    finishPlayer();
                } else if (player.autoAdvancePending) {
                    player.playbackFinishedDuringAdvance = true;
                } else if (player.card.type === 'Episode' && player.seriesPlaybackSetting.autoNext) {
                    void advanceToNextEpisode(player, 'finished');
                } else {
                    finishPlayer();
                }
                break;
            case 'canceled':
                finishPlayer();
                break;
            case 'error':
                if (player.exiting) { finishPlayer(); break; }
                showPlaybackFailure(event.errorCode
                    ? friendlyError({ code: event.errorCode })
                    : (player.interpolationChanging
                        ? 'RTX 插帧切换失败'
                        : (player.episodeChanging ? '剧集切换失败' : '播放失败')));
                break;
        }
    }

    function setPlayerLoading(visible, label = '') {
        if (label) byId('player-loading-label').textContent = label;
        byId('player-loading').classList.toggle('hidden', !visible);
        const covered = visible && (!player?.started
            || player?.interpolationChanging
            || player?.episodeChanging
            || player?.autoAdvancePending
            || player?.exiting);
        playerView.classList.toggle('preparing', covered);
        playerView.setAttribute('aria-busy', String(visible));
        playerControls.querySelector('.player-bottom').inert = covered;
    }

    function showPlaybackFailure(message) {
        if (!player) return;
        const failed = { card: player.card, positionMs: player.positionMs, mediaSourceId: player.mediaSourceId || '' };
        finishPlayer(); playbackFailure = failed;
        setPlayerLoading(false); playerView.classList.remove('hidden'); playerView.classList.add('preparing');
        playerControls.classList.remove('visible'); playerControls.inert = true;
        byId('player-failure-message').textContent = `${cardTitle(failed.card, true)} · ${message}`;
        byId('player-failure').classList.remove('hidden'); setPlayerMode(true);
        byId('player-retry').focus();
    }

    function dismissPlaybackFailure() {
        if (!playbackFailure) return;
        playbackFailure = null; byId('player-failure').classList.add('hidden');
        playerView.classList.add('hidden'); playerView.classList.remove('preparing'); setPlayerMode(false);
    }

    function setPlayerPoster(card) {
        const targetPlayer = player;
        const img = byId('player-poster').firstElementChild;
        const ref = card.backdropImage || card.landscapeImage || card.primaryImage;
        img.removeAttribute('src'); img.classList.remove('image-ready', 'image-error');
        const cover = byId('player-loading-art'); cover.removeAttribute('src');
        const detailCard = currentView?.kind === 'detail' ? currentView.data.item : null;
        const knownCards = [detailCard, ...(homeData?.latest || []), ...(homeData?.latestSections || []).flatMap((section) => section.items || []), ...[...libraryCache.values()].flatMap((entry) => entry.items || [])];
        const series = card.type === 'Episode' ? knownCards.find((item) => item?.id === card.seriesId) : null;
        const coverRef = card.type === 'Episode' ? series?.primaryImage : card.primaryImage;
        cover.classList.remove('image-ready', 'image-error'); cover.hidden = !coverRef;
        if (coverRef) requestImage(coverRef, 360).then((src) => {
            if (player === targetPlayer && player?.card?.id === card.id) setImageSource(cover, src);
        }).catch((error) => console.error('播放封面加载失败：' + friendlyError(error)));
        if (!ref) return;
        requestImage(ref, 1280)
            .then((src) => {
                if (img.isConnected && player === targetPlayer && player?.card?.id === card.id) {
                    setImageSource(img, src);
                }
            })
            .catch((error) => console.error(`播放器背景加载失败：${friendlyError(error)}`));
    }

    function waitForPlayerPaint() {
        return new Promise((resolve) => {
            requestAnimationFrame(() => requestAnimationFrame(resolve));
        });
    }

    function interpolationLoadingLabel(activePlayer) {
        const target = activePlayer?.interpolationTargetModel;
        return target
            ? `正在加载${interpolationModels[target]?.label || ''}插帧`
            : '正在关闭 RTX 插帧';
    }

    function playerTransitionLoadingLabel(activePlayer) {
        if (activePlayer?.episodeChanging && activePlayer.episodeTarget) {
            return `正在加载${episodePosition(activePlayer.episodeTarget).replace(' · ', '')}`;
        }
        return interpolationLoadingLabel(activePlayer);
    }

    function updatePlayerProgress() {
        const durationMs = player?.durationMs || 0;
        const positionMs = player?.scrubbing && Number.isFinite(player.scrubPositionMs)
            ? player.scrubPositionMs
            : player?.positionMs || 0;
        byId('player-current').textContent = formatTime(positionMs);
        byId('player-duration').textContent = formatTime(durationMs);
        const value = durationMs ? Math.round(positionMs / durationMs * 1000) : 0;
        const clamped = Math.max(0, Math.min(1000, value));
        const progress = byId('player-progress');
        progress.value = String(clamped);
        progress.style.setProperty('--player-progress', `${clamped / 10}%`);
    }

    function progressTargetMs(value) {
        if (!player?.durationMs) return null;
        const ratio = Number(value) / 1000;
        if (!Number.isFinite(ratio)) return null;
        return Math.max(0, Math.min(player.durationMs, ratio * player.durationMs));
    }

    function previewProgressSeek(value) {
        const target = progressTargetMs(value);
        if (target === null || !player || player.exiting) return;
        player.scrubbing = true;
        player.scrubPositionMs = target;
        updatePlayerProgress();
    }

    function commitProgressSeek(value) {
        const target = progressTargetMs(value);
        if (target === null || !player || player.exiting || !window.jmpNative) {
            cancelProgressSeek();
            return;
        }
        player.scrubbing = false;
        player.scrubPositionMs = null;
        player.positionMs = target;
        updatePlayerProgress();
        window.jmpNative.playerSeek(Math.round(target));
    }

    function cancelProgressSeek() {
        if (!player) return;
        player.scrubbing = false;
        player.scrubPositionMs = null;
        updatePlayerProgress();
    }

    function togglePlayback() {
        if (!player || player.exiting || !window.jmpNative) return;
        if (player.interpolationChanging) {
            player.interpolationResumePlaying = !player.interpolationResumePlaying;
            player.playing = player.interpolationResumePlaying;
            hidePlayerFeedback();
            showPlayerControls();
            refreshPlayerTools();
            return;
        }
        if (player.playing) {
            window.jmpNative.playerPause();
            player.playing = false;
            hidePlayerFeedback();
            showPlayerControls();
        } else {
            window.jmpNative.playerPlay();
            player.playing = true;
            hidePlayerFeedback();
            showPlayerControls();
        }
        refreshPlayerTools();
        if (player.started) showPlayerFeedback({ icon: player.playing ? 'play' : 'pause' }, 750);
    }

    const videoEnhancementDefaults = Object.freeze({ vsr: false, vfg: false, nr: false, vsrQuality: 4, vsrTarget: 0, vfgQuality: 0, multiplier: 2, sceneDetection: true, nrStyle: 0, nrTarget: 1080, nrIntensity: 1, nrTone: 1, nrStructure: 1, nrSkin: -1, nrAutoMask: false, nrUiCorrection: false });
    function videoEnhancementSelection(value) { return { ...videoEnhancementDefaults, ...value }; }
    function sameVideoEnhancement(left, right) {
        const a = videoEnhancementSelection(left), b = videoEnhancementSelection(right);
        return Object.keys(videoEnhancementDefaults).every((key) => typeof a[key] === 'number' && typeof b[key] === 'number' ? Math.abs(a[key] - b[key]) < 0.00001 : a[key] === b[key]);
    }
    async function setFrameInterpolation(modelId, enhancement = player?.videoEnhancement) {
        const activePlayer = player;
        if (!activePlayer?.started || !activePlayer.loadInfo
            || activePlayer.interpolationChanging || activePlayer.exiting) return;
        if (modelId !== null && !Object.prototype.hasOwnProperty.call(interpolationModels, modelId)) {
            const error = new Error('所选 RIFE 模型无效');
            error.code = 'frame_interpolation_model_invalid';
            showToast(friendlyError(error));
            return;
        }
        const requestedModel = modelId;
        const requestedEnhancement = videoEnhancementSelection(enhancement);
        if (modelId) requestedEnhancement.vfg = false;
        const activeModel = activePlayer.interpolationEnabled
            ? (activePlayer.interpolationModel || activePlayer.loadInfo.frameInterpolation?.modelId)
            : null;
        if (requestedModel === activeModel && sameVideoEnhancement(activePlayer.videoEnhancement, requestedEnhancement)) {
            closePlayerPanel(false);
            return;
        }
        const previousLoadInfo = activePlayer.loadInfo;
        const previousModel = activePlayer.interpolationModel;
        const reloadPositionMs = Math.max(0, Math.round(activePlayer.positionMs || 0));
        activePlayer.interpolationChanging = true;
        activePlayer.interpolationTargetModel = requestedModel;
        activePlayer.interpolationResumePlaying = activePlayer.playing;
        if (activePlayer.playing && window.jmpNative) window.jmpNative.playerPause();
        closePlayerPanel(false);
        setPlayerLoading(true, playerTransitionLoadingLabel(activePlayer));
        showPlayerControls();
        refreshPlayerTools();
        try {
            await waitForPlayerPaint();
            if (requestedModel) {
                const status = await nativeRequest(
                    'mediaStationFrameInterpolation',
                    'frame_interpolation_set_model',
                    ['frame_interpolation_set_model', requestedModel],
                    15000,
                );
                if (player !== activePlayer || activePlayer.exiting) return;
                preferredInterpolationModel = requestedModel;
                updateFrameInterpolationStatus(status);
            }
            const loadInfo = await nativeRequest(
                'mediaStationLoad',
                'load',
                [
                    activePlayer.card.id,
                    reloadPositionMs,
                    requestedModel ? '2x' : 'off',
                    requestedModel || preferredInterpolationModel,
                    playbackPreferenceScope(activePlayer.card),
                    playerSubtitleStyle.fontSize,
                    playerSubtitlePosition(),
                    activePlayer.loadInfo.mediaSourceId || '',
                    JSON.stringify(requestedEnhancement),
                ],
                60000,
            );
            if (player !== activePlayer || activePlayer.exiting) return;
            if (!loadInfo.videoEnhancement || !Object.keys(requestedEnhancement).every((key) => Object.prototype.hasOwnProperty.call(loadInfo.videoEnhancement, key)) || !sameVideoEnhancement(loadInfo.videoEnhancement, requestedEnhancement)) throw new Error('播放器返回的画面增强参数与请求不一致');
            const actualModel = loadInfo.frameInterpolation?.modelId || null;
            if (actualModel !== requestedModel) {
                const error = new Error('播放器返回的 RTX 插帧状态与请求不一致');
                error.code = 'frame_interpolation_state_mismatch';
                throw error;
            }
            activePlayer.loadInfo = loadInfo;
            activePlayer.videoEnhancement = loadInfo.videoEnhancement;
            applyPlayerPlaybackRate(playerPlaybackRate);
            if (activePlayer.interpolationChanging) {
                setPlayerLoading(true, playerTransitionLoadingLabel(activePlayer));
                if (window.jmpNative) window.jmpNative.playerPlay();
            }
            refreshPlayerTools();
        } catch (error) {
            if (player !== activePlayer || activePlayer.exiting) return;
            activePlayer.interpolationChanging = false;
            activePlayer.interpolationTargetModel = null;
            activePlayer.interpolationModel = previousModel;
            activePlayer.loadInfo = previousLoadInfo;
            activePlayer.playing = activePlayer.interpolationResumePlaying;
            if (activePlayer.interpolationResumePlaying && window.jmpNative) {
                window.jmpNative.playerPlay();
            }
            setPlayerLoading(activePlayer.buffering, activePlayer.buffering ? '正在缓冲' : '');
            refreshPlayerTools();
            showToast(friendlyError(error));
        }
    }

    function togglePlayerFullscreen() {
        if (!player || player.exiting || !window.jmpNative) return;
        window.jmpNative.toggleFullscreen();
        showPlayerControls();
    }

    function isPlayerInteractiveTarget(target) {
        return target instanceof Element && Boolean(target.closest('button, input, .player-panel, .player-danmaku-panel'));
    }

    function seekBy(deltaMs) {
        if (!player || player.exiting || !window.jmpNative || !player.durationMs) return;
        const target = Math.max(0, Math.min(player.durationMs, player.positionMs + deltaMs));
        player.positionMs = target;
        updatePlayerProgress();
        window.jmpNative.playerSeek(Math.round(target));
    }

    function persistPlayerPlaybackSettings() {
        const scopeId = player?.playbackSettingsScopeId;
        if (!scopeId) {
            console.error('MediaStation playback settings have no active media scope');
            showToast('播放设置保存失败');
            return;
        }
        const nextSettings = {
            ...playerPlaybackSettings,
            [scopeId]: {
                volume: playerVolume,
                lastAudibleVolume: lastAudiblePlayerVolume,
                subtitleStyle: playerSubtitleStyleCustomized ? { ...playerSubtitleStyle } : null,
            },
        };
        try {
            window.localStorage.setItem(playerPlaybackSettingsKey, JSON.stringify(nextSettings));
            playerPlaybackSettings = nextSettings;
        } catch (error) {
            console.error(`MediaStation playback settings could not be saved: ${error}`);
            showToast('播放设置保存失败');
        }
    }

    function applyPlayerVolume(value, persist = false) {
        const normalized = Math.max(0, Math.min(100, Math.round(Number(value) || 0)));
        playerVolume = normalized;
        if (normalized > 0) lastAudiblePlayerVolume = normalized;
        if (window.jmpNative) {
            window.jmpNative.playerSetVolume(normalized);
            window.jmpNative.playerSetMuted(normalized === 0);
        }
        if (persist) persistPlayerPlaybackSettings();
        refreshPlayerVolume();
    }

    function togglePlayerMuted() {
        applyPlayerVolume(playerVolume > 0 ? 0 : lastAudiblePlayerVolume, true);
        showPlayerControls();
    }

    function applyPlayerSubtitleStyle(style, persist = false, customized = playerSubtitleStyleCustomized) {
        const fontSize = Math.round(Number(style?.fontSize));
        const bottomOffset = Math.round(Number(style?.bottomOffset));
        if (!Number.isFinite(fontSize) || fontSize < 24 || fontSize > 64
            || !Number.isFinite(bottomOffset) || bottomOffset < 0 || bottomOffset > 30) {
            throw new RangeError('Unsupported subtitle style');
        }
        playerSubtitleStyle = { fontSize, bottomOffset };
        playerSubtitleStyleCustomized = customized;
        if (window.jmpNative?.playerSetSubtitleStyle) {
            window.jmpNative.playerSetSubtitleStyle(fontSize, playerSubtitlePosition());
        }
        if (persist) persistPlayerPlaybackSettings();
    }

    function refreshPlayerVolume() {
        const slider = byId('player-volume');
        const button = byId('player-volume-toggle');
        const muted = playerVolume === 0;
        slider.value = String(playerVolume);
        slider.style.setProperty('--player-volume', `${playerVolume}%`);
        byId('player-volume-value').value = String(playerVolume);
        button.classList.toggle('is-muted', muted);
        button.title = muted ? '取消静音' : '静音';
        button.setAttribute('aria-label', button.title);
        button.setAttribute('aria-pressed', String(muted));
    }

    function formatPlayerPlaybackRate(rate) {
        return `${Number(rate).toLocaleString('zh-CN', { maximumFractionDigits: 2 })}×`;
    }

    function applyPlayerPlaybackRate(rate) {
        const normalized = Number(rate);
        if (!playerPlaybackRates.includes(normalized)) {
            throw new RangeError(`Unsupported playback rate: ${rate}`);
        }
        playerPlaybackRate = normalized;
        danmakuRenderer.setRate(normalized);
        if (window.jmpNative) window.jmpNative.playerSetSpeed(Math.round(normalized * 1000));
        refreshPlayerPlaybackRate();
    }

    function refreshPlayerPlaybackRate() {
        const button = byId('player-speed');
        const label = formatPlayerPlaybackRate(playerPlaybackRate);
        button.title = `播放速度：${label}`;
        button.setAttribute('aria-label', button.title);
        const caption = button.querySelector('.player-tool-label');
        if (caption) caption.textContent = label;
    }

    function selectPlayerPlaybackRate(rate) {
        applyPlayerPlaybackRate(rate);
        closePlayerPanel(false);
        showPlayerFeedback(formatPlayerPlaybackRate(rate));
        showPlayerControls();
    }

    function togglePlayerDanmaku() {
        if (!player || player.exiting || !player.loadInfo) return;
        const enabled = !danmakuEnabled;
        applyDanmakuEnabled(enabled, true);
        showPlayerControls();
        showPlayerFeedback(`弹幕${enabled ? '已开启' : '已关闭'}`, 2000);
    }

    function refreshPlayerTools() {
        const info = player?.loadInfo;
        const playback = byId('player-playback');
        const episodes = byId('player-episodes');
        const danmaku = byId('player-danmaku');
        const subtitles = byId('player-subtitles');
        const audio = byId('player-audio');
        const speed = byId('player-speed');
        const interpolation = byId('player-interpolation');
        const infoButton = byId('player-info');
        const fullscreen = byId('player-fullscreen');
        const exit = byId('player-exit');
        const progress = byId('player-progress');
        const exiting = player?.exiting === true;
        const episodeChanging = player?.episodeChanging === true;
        const autoAdvancePending = player?.autoAdvancePending === true;
        const trackChanging = player?.trackChanging === true;
        const playing = player?.playing === true;
        const fullscreenActive = window._isFullscreen === true;
        exit.disabled = !player || exiting;
        progress.disabled = !player || exiting || autoAdvancePending;
        playback.disabled = !player || exiting || episodeChanging || autoAdvancePending;
        playback.classList.toggle('is-playing', playing);
        playback.title = playing ? '暂停' : '播放';
        playback.setAttribute('aria-label', playback.title);
        byId('player-seek-back').disabled = !player?.started || exiting || episodeChanging || autoAdvancePending;
        byId('player-seek-forward').disabled = byId('player-seek-back').disabled;
        byId('player-more').disabled = !info || exiting || episodeChanging || autoAdvancePending;
        const next = byId('player-next'); next.classList.toggle('hidden', player?.card?.type !== 'Episode');
        next.disabled = !info || !player?.started || exiting || episodeChanging || autoAdvancePending || player?.episodesLoading
            || (Array.isArray(player?.episodes) && !nextPlayerEpisode(player));
        const episodic = player?.card?.type === 'Episode' && Boolean(player.card.seriesId);
        episodes.classList.toggle('hidden', !episodic);
        episodes.disabled = exiting || episodeChanging || autoAdvancePending || trackChanging || !info || !episodic || player?.episodesLoading;
        episodes.title = player?.episodesLoading ? '正在读取选集' : '选集';
        episodes.setAttribute('aria-label', episodes.title);
        danmaku.disabled = !player || exiting || episodeChanging || autoAdvancePending || !info;
        danmaku.title = `弹幕：${danmakuEnabled ? '显示' : '关闭'}（悬停配置）`;
        danmaku.setAttribute('aria-label', danmaku.title);
        subtitles.disabled = exiting || episodeChanging || autoAdvancePending || trackChanging || !info || !Array.isArray(info.subtitleTracks);
        audio.disabled = exiting || episodeChanging || autoAdvancePending || trackChanging || !info || !Array.isArray(info.audioTracks) || !info.audioTracks.length;
        speed.disabled = !player || exiting || episodeChanging || autoAdvancePending || trackChanging || !player.started;
        const interpolationEnabled = player?.interpolationChanging
            ? player.interpolationTargetModel !== null
            : player?.interpolationEnabled === true;
        const interpolationModel = player?.interpolationChanging
            ? player.interpolationTargetModel
            : (player?.interpolationModel || info?.frameInterpolation?.modelId || null);
        const interpolationInstalled = interpolation.dataset.componentInstalled === 'true' || [...videoComponentStatuses.values()].some((status) => status.installed === true && !status.runtime?.experimental);
        const interpolationReady = interpolation.dataset.componentReady === 'true' || interpolation.dataset.enhancementReady === 'true';
        interpolation.classList.toggle('hidden', !interpolationInstalled);
        interpolation.disabled = !interpolationReady || exiting || episodeChanging || autoAdvancePending || trackChanging || !player?.started || !info || player.interpolationChanging;
        interpolation.title = player?.interpolationChanging
            ? playerTransitionLoadingLabel(player)
            : (!interpolationReady
                ? '画面增强组件未就绪'
                : interpolationEnabled
                ? `RTX 插帧：${interpolationModels[interpolationModel]?.label || '已开启'}`
                : (player?.videoEnhancement?.vfg ? 'NVIDIA VFG 插帧' : player?.videoEnhancement?.vsr ? 'RTX Video 超分' : player?.videoEnhancement?.nr ? 'DLSSNR 画面增强' : '画面增强'));
        interpolation.setAttribute('aria-label', interpolation.title);
        interpolation.setAttribute('aria-pressed', String(interpolationEnabled || player?.videoEnhancement?.vsr || player?.videoEnhancement?.vfg || player?.videoEnhancement?.nr || false));
        infoButton.disabled = exiting || episodeChanging || autoAdvancePending || trackChanging || !info;
        fullscreen.disabled = !player || exiting || episodeChanging || autoAdvancePending;
        fullscreen.classList.toggle('is-fullscreen', fullscreenActive);
        fullscreen.title = fullscreenActive ? '退出全屏' : '进入全屏';
        fullscreen.setAttribute('aria-label', fullscreen.title);
        fullscreen.setAttribute('aria-pressed', String(fullscreenActive));
        const activeKind = player?.panelKind || '';
        episodes.setAttribute('aria-pressed', String(activeKind === 'episodes'));
        danmaku.setAttribute('aria-pressed', String(danmakuEnabled));
        subtitles.setAttribute('aria-pressed', String(activeKind === 'subtitles'));
        audio.setAttribute('aria-pressed', String(activeKind === 'audio'));
        speed.setAttribute('aria-pressed', String(activeKind === 'speed'));
        infoButton.setAttribute('aria-pressed', String(activeKind === 'info'));
        refreshPlayerVolume();
        refreshPlayerPlaybackRate();
    }

    function localizedLanguageName(value) {
        const raw = String(value || '').trim();
        if (!raw) return '';
        const normalized = raw.toLowerCase().replaceAll('_', '-');
        if (['und', 'unknown', '未知'].includes(normalized)) return '';
        if (/chinese\s*\(simplified\)|simplified\s+chinese|chinese\s+simplified/.test(normalized)) return '简体中文';
        if (/chinese\s*\(traditional\)|traditional\s+chinese|chinese\s+traditional/.test(normalized)) return '繁体中文';
        if (['zh-cn', 'zh-sg', 'zh-hans', 'chs', 'sc'].includes(normalized)) return '简体中文';
        if (['zh-tw', 'zh-hk', 'zh-mo', 'zh-hant', 'cht', 'tc'].includes(normalized)) return '繁体中文';
        if (['zh', 'zho', 'chi', 'chinese', 'cn'].includes(normalized)) return '中文';
        return languageNameMap[normalized] || languageNameMap[normalized.split('-')[0]] || raw;
    }

    function localizedTrackLabel(value) {
        let label = String(value || '').trim();
        if (!label) return '';
        label = label
            .replace(/chinese\s*\(simplified\)|simplified\s+chinese|chinese\s+simplified/gi, '简体中文')
            .replace(/chinese\s*\(traditional\)|traditional\s+chinese|chinese\s+traditional/gi, '繁体中文')
            .replace(/\bzh[-_](?:cn|sg|hans)\b|\bchs\b/gi, '简体中文')
            .replace(/\bzh[-_](?:tw|hk|mo|hant)\b|\bcht\b/gi, '繁体中文')
            .replace(/\b(?:chinese|zho|chi|zh)\b/gi, '中文');
        return label;
    }

    function trackTitle(track, fallback) {
        return localizedTrackLabel(track?.label)
            || localizedLanguageName(track?.language)
            || fallback;
    }

    function subtitleTrackTitle(track, fallback) {
        const rawLabel = String(track?.label || '').trim();
        const normalizedLabel = rawLabel.toLowerCase();
        let language = localizedLanguageName(track?.language);
        if (!language) {
            const labelLanguage = localizedLanguageName(rawLabel);
            if (labelLanguage && labelLanguage !== rawLabel) language = labelLanguage;
        }
        if (!language) {
            const localizedLabel = localizedTrackLabel(rawLabel);
            if (/简体中文|繁体中文|中文/.test(localizedLabel)) language = localizedLabel;
        }
        const qualifiers = [];
        if (/\b(?:sdh|hi)\b|hearing[\s_-]*impaired|closed[\s_-]*captions?/.test(normalizedLabel)) qualifiers.push('听障');
        else if (/\bfull\b|complete/.test(normalizedLabel)) qualifiers.push('完整');
        if (track?.forced && !qualifiers.includes('强制')) qualifiers.push('强制');
        const title = language || fallback;
        return qualifiers.length ? `${title}（${qualifiers.join(' · ')}）` : title;
    }

    function trackMeta(track, kind) {
        const parts = [];
        const language = localizedLanguageName(track?.language);
        const label = kind === 'subtitle' ? '' : localizedTrackLabel(track?.label);
        if (kind !== 'subtitle' && language && !label.includes(language)) parts.push(language);
        if (track?.codec) parts.push(String(track.codec).toUpperCase());
        if (kind === 'audio' && track?.channels) parts.push(`${track.channels} 声道`);
        if (kind === 'subtitle') {
            parts.push(track.external ? '外置' : '内置');
            if (track.forced) parts.push('强制');
            else if (track.default) parts.push('默认');
        }
        return parts.join(' · ');
    }

    function createTrackOption({ title, meta, selected, onSelect }) {
        const option = element('button', 'track-option');
        option.type = 'button';
        option.setAttribute('role', 'radio');
        option.setAttribute('aria-checked', String(selected));
        const copy = element('span');
        copy.append(element('strong', '', title));
        if (meta) copy.append(element('small', '', meta));
        option.append(copy);
        option.append(element('span', 'selection-mark', selected ? '✓' : ''));
        option.addEventListener('click', onSelect);
        return option;
    }

    function createPlayerEpisodeOption(episode, selected, onSelect) {
        const option = element('button', 'player-episode-option');
        option.type = 'button';
        option.setAttribute('role', 'radio');
        option.setAttribute('aria-checked', String(selected));
        const optionTitle = episodeOptionTitle(episode);
        option.setAttribute('aria-label', selected ? `${optionTitle}，正在播放` : optionTitle);

        const art = element('span', 'card-art');
        const fallback = element('span', 'art-fallback');
        const image = document.createElement('img');
        image.alt = '';
        image.decoding = 'async';
        art.append(fallback, image);
        const ref = episode.landscapeImage || episode.primaryImage;
        observeImage(image, ref, imageWidthFor(ref, true));
        const copy = element('span', 'player-episode-copy');
        copy.append(element('strong', '', optionTitle), element('small', '', selected ? '当前单集' : formatDuration(episode.durationMs) || '时长未知'));
        option.append(art, copy);
        if (selected) option.append(strokeIcon('audio'));
        option.addEventListener('click', onSelect);
        return option;
    }

    function createSubtitleStyleControl({ label, property, min, max, suffix }) {
        const row = element('label', 'player-subtitle-style-row');
        const input = element('input', 'player-panel-control');
        input.type = 'range';
        input.min = String(min);
        input.max = String(max);
        input.step = '1';
        input.value = String(playerSubtitleStyle[property]);
        input.setAttribute('aria-label', label);
        const output = element('output', '', `${playerSubtitleStyle[property]}${suffix}`);
        const refresh = () => {
            const value = Number(input.value);
            input.style.setProperty('--subtitle-style-progress', `${(value - min) / (max - min) * 100}%`);
            output.value = `${value}${suffix}`;
        };
        input.addEventListener('input', () => {
            refresh();
            applyPlayerSubtitleStyle({ ...playerSubtitleStyle, [property]: Number(input.value) }, false, true);
        });
        input.addEventListener('change', () => {
            applyPlayerSubtitleStyle({ ...playerSubtitleStyle, [property]: Number(input.value) }, true, true);
        });
        refresh();
        row.append(element('span', '', label), input, output);
        return row;
    }

    function createSubtitleStyleControls() {
        const section = element('section', 'player-subtitle-style');
        const heading = element('div', 'player-subtitle-style-heading');
        const reset = element('button', 'player-subtitle-style-reset player-panel-control', '使用默认');
        reset.type = 'button';
        reset.addEventListener('click', () => {
            applyPlayerSubtitleStyle(defaultPlayerSubtitleStyle, true, false);
            renderPlayerPanel();
        });
        const title = element('span');
        title.append(
            element('strong', '', '字幕样式'),
            element('small', 'player-subtitle-style-mode', playerSubtitleStyleCustomized ? '当前影片自定义' : '使用设置默认'),
        );
        heading.append(title, reset);
        section.append(
            heading,
            createSubtitleStyleControl({ label: '文字大小', property: 'fontSize', min: 24, max: 64, suffix: '' }),
            createSubtitleStyleControl({ label: '距底部', property: 'bottomOffset', min: 0, max: 30, suffix: '%' }),
            element('p', 'player-subtitle-style-note', '样式调整适用于 ASS、SRT 等文本字幕；PGS 等图形字幕保持原样。'),
        );
        return section;
    }

    function infoValue(value, fallback = '-') {
        if (value === null || value === undefined || value === '') return fallback;
        return String(value);
    }

    function appendInfoRow(list, label, value) {
        const term = element('dt', '', label);
        const detail = element('dd', '', value);
        list.append(term, detail);
    }

    function createInfoSection(title) {
        const section = element('section', 'player-info-section');
        const list = element('dl', 'player-info-grid');
        section.append(element('h3', '', title), list);
        return { section, list };
    }

    function episodeSeasonNumber(episode) {
        return Number.isFinite(episode?.parentIndexNumber) ? episode.parentIndexNumber : 0;
    }

    function episodeSeasonLabel(season) {
        return season === 0 ? '特别篇' : `第 ${season} 季`;
    }

    function episodeOptionTitle(episode) {
        const index = Number.isFinite(episode?.indexNumber) ? `第 ${episode.indexNumber} 集` : '剧集';
        return episode?.title ? `${index} · ${episode.title}` : index;
    }

    function formatSeriesSkipSeconds(seconds) {
        return seconds > 0 ? `${seconds} 秒` : '关闭';
    }

    function applyActiveSeriesPlaybackSetting(setting) {
        const activePlayer = player;
        const seriesId = activePlayer?.card?.seriesId;
        if (!activePlayer || activePlayer.card.type !== 'Episode' || !seriesId) return false;
        if (!persistSeriesPlaybackSetting(seriesId, setting)) return false;
        activePlayer.seriesPlaybackSetting = { ...setting };
        const introEndMs = setting.introSkipSeconds * 1000;
        activePlayer.introSkipHandled = introEndMs === 0 || activePlayer.positionMs >= introEndMs;
        activePlayer.outroSkipHandled = false;
        activePlayer.automationDisabled = false;
        activePlayer.automationWarningShown = false;
        return true;
    }

    function createSeriesPlaybackTimeControl({ label, property }) {
        const row = element('label', 'player-series-time-row');
        const input = element('input', 'player-panel-control');
        input.type = 'range';
        input.min = '0';
        input.max = '600';
        input.step = '5';
        input.value = String(player.seriesPlaybackSetting[property]);
        input.setAttribute('aria-label', label);
        const output = element('output', '', formatSeriesSkipSeconds(Number(input.value)));
        const refresh = () => {
            const value = Number(input.value);
            input.style.setProperty('--series-setting-progress', `${value / 600 * 100}%`);
            output.value = formatSeriesSkipSeconds(value);
        };
        input.addEventListener('input', refresh);
        input.addEventListener('change', () => {
            const previous = player?.seriesPlaybackSetting?.[property];
            const next = { ...player.seriesPlaybackSetting, [property]: Number(input.value) };
            if (!applyActiveSeriesPlaybackSetting(next)) {
                input.value = String(previous ?? 0);
                refresh();
            }
        });
        refresh();
        row.append(element('span', '', label), input, output);
        return { row, input };
    }

    function createSeriesPlaybackControls() {
        const section = element('section', 'player-series-playback');
        const heading = element('div', 'player-series-playback-heading');
        heading.append(
            element('strong', '', '连续播放'),
            element('small', '', '当前整部剧'),
        );
        const toggleRow = element('label', 'player-series-toggle-row');
        const toggle = element('input', 'player-panel-control player-series-toggle');
        toggle.type = 'checkbox';
        toggle.checked = player.seriesPlaybackSetting.autoNext;
        toggle.setAttribute('aria-label', '自动播放下一集');
        toggleRow.append(element('span', '', '自动播放下一集'), toggle);
        const intro = createSeriesPlaybackTimeControl({ label: '跳过片头', property: 'introSkipSeconds' });
        const outro = createSeriesPlaybackTimeControl({ label: '跳过片尾', property: 'outroSkipSeconds' });
        outro.input.disabled = !toggle.checked;
        toggle.addEventListener('change', () => {
            const previous = player?.seriesPlaybackSetting?.autoNext === true;
            const next = { ...player.seriesPlaybackSetting, autoNext: toggle.checked };
            if (!applyActiveSeriesPlaybackSetting(next)) toggle.checked = previous;
            outro.input.disabled = !toggle.checked;
        });
        section.append(
            heading,
            toggleRow,
            intro.row,
            outro.row,
            element('p', 'player-series-playback-note', '0 秒表示关闭；自动连播保留当前倍速，手动选集恢复 1×。'),
        );
        return section;
    }

    function renderPlayerPanel() {
        const info = player?.loadInfo;
        const kind = player?.panelKind;
        playerPanelContent.replaceChildren();
        requestAnimationFrame(positionPlayerPanel);
        if (!info || !kind) return;

        if (kind === 'episodes') {
            byId('player-panel-title').textContent = '选集';
            if (player.episodesLoading) {
                playerPanelContent.append(element('div', 'player-panel-state', '正在读取剧集...'));
                return;
            }
            if (player.episodesError) {
                playerPanelContent.append(element('div', 'player-panel-state', player.episodesError));
                return;
            }
            const allEpisodes = Array.isArray(player.episodes) ? player.episodes : [];
            if (!allEpisodes.length) {
                playerPanelContent.append(element('div', 'player-panel-state', '没有可播放的剧集'));
                return;
            }
            const seasons = [...new Set(allEpisodes.map(episodeSeasonNumber))].sort((left, right) => left - right);
            if (!seasons.includes(player.episodeSeason)) {
                player.episodeSeason = episodeSeasonNumber(player.card);
            }
            if (!seasons.includes(player.episodeSeason)) player.episodeSeason = seasons[0];
            if (seasons.length > 1) {
                const tabs = element('div', 'player-season-tabs');
                tabs.setAttribute('role', 'tablist');
                for (const season of seasons) {
                    const tab = element('button', 'player-season-tab', episodeSeasonLabel(season));
                    tab.type = 'button';
                    tab.setAttribute('role', 'tab');
                    tab.setAttribute('aria-selected', String(player.episodeSeason === season));
                    tab.addEventListener('click', () => {
                        if (!player || player.episodeSeason === season) return;
                        player.episodeSeason = season;
                        renderPlayerPanel();
                        requestAnimationFrame(() => focusElement(playerPanelContent.querySelector('.player-episode-option[aria-checked="true"], .player-episode-option')));
                    });
                    tabs.append(tab);
                }
                playerPanelContent.append(tabs);
            }
            const group = element('div', 'player-episode-grid');
            group.setAttribute('role', 'radiogroup');
            const episodes = allEpisodes
                .filter((episode) => episodeSeasonNumber(episode) === player.episodeSeason)
                .sort((left, right) => (left.indexNumber || 0) - (right.indexNumber || 0));
            for (const episode of episodes) {
                group.append(createPlayerEpisodeOption(episode, player.card.id === episode.id, () => switchPlayerEpisode(episode)));
            }
            playerPanelContent.append(group, createSeriesPlaybackControls());
            return;
        }

        if (kind === 'subtitles') {
            byId('player-panel-title').textContent = '字幕';
            const group = element('div');
            group.setAttribute('role', 'radiogroup');
            const enabled = info.subtitleEnabled === true;
            group.append(createTrackOption({
                title: '关闭字幕',
                meta: '',
                selected: !enabled,
                onSelect: () => selectPlayerTrack('subtitle', ''),
            }));
            for (let index = 0; index < info.subtitleTracks.length; index += 1) {
                const track = info.subtitleTracks[index];
                group.append(createTrackOption({
                    title: subtitleTrackTitle(track, `字幕 ${index + 1}`),
                    meta: trackMeta(track, 'subtitle'),
                    selected: enabled && info.subtitleTrackKey === track.key,
                    onSelect: () => selectPlayerTrack('subtitle', track.key),
                }));
            }
            playerPanelContent.append(createSubtitleStyleControls(), group);
            return;
        }

        if (kind === 'audio') {
            byId('player-panel-title').textContent = '音轨';
            const group = element('div');
            for (let index = 0; index < info.audioTracks.length; index += 1) {
                const track = info.audioTracks[index];
                group.append(createTrackOption({
                    title: trackTitle(track, `音轨 ${index + 1}`),
                    meta: trackMeta(track, 'audio'),
                    selected: info.audioTrackKey === track.key,
                    onSelect: () => selectPlayerTrack('audio', track.key),
                }));
            }
            playerPanelContent.append(group);
            return;
        }

        if (kind === 'speed') {
            byId('player-panel-title').textContent = '播放速度';
            const group = element('div');
            group.setAttribute('role', 'radiogroup');
            for (const rate of playerPlaybackRates) {
                group.append(createTrackOption({
                    title: formatPlayerPlaybackRate(rate),
                    meta: rate === 1 ? '正常速度' : '',
                    selected: playerPlaybackRate === rate,
                    onSelect: () => selectPlayerPlaybackRate(rate),
                }));
            }
            playerPanelContent.append(group);
            return;
        }

        if (kind === 'interpolation') {
            byId('player-panel-title').textContent = '画面增强';
            const selectedModel = player.interpolationChanging
                ? player.interpolationTargetModel
                : (player.interpolationEnabled
                    ? (player.interpolationModel || info.frameInterpolation?.modelId)
                    : null);
            const group = element('div');
            group.append(createTrackOption({
                title: '关闭插帧',
                meta: '超分保持当前选择',
                selected: selectedModel === null && !player.videoEnhancement?.vfg,
                onSelect: () => setFrameInterpolation(null, { ...videoEnhancementSelection(player.videoEnhancement), vfg: false }),
            }));
            for (const [modelId, model] of Object.entries(interpolationModels)) {
                const option = createTrackOption({ title: model.label, meta: model.name, selected: selectedModel === modelId, onSelect: () => setFrameInterpolation(modelId) });
                option.disabled = byId('player-interpolation').dataset.componentReady !== 'true'; group.append(option);
            }
            playerPanelContent.append(group);
            const draft = videoEnhancementSelection(player.videoEnhancement);
            for (const [id, key, title, meta] of [['rtx-video', 'vsr', 'RTX Video 超分', 'SDR / HDR10 · 质量与目标尺寸可配置'], ['nvidia-vfg', 'vfg', 'NVIDIA VFG 插帧', 'SDR / HDR10 · 2–8 倍帧率'], ['dlssnr', 'nr', 'DLSSNR 画面增强', 'SDR / HDR10 · 风格与增强强度可配置']]) {
                const ready = videoComponentStatuses.get(id)?.runtime?.playbackAvailable === true;
                const selected = player.videoEnhancement?.[key] === true;
                const button = createTrackOption({ title, meta: ready ? meta : '请先在组件管理安装', selected, onSelect: () => {
                    const next = { ...draft, [key]: !selected };
                    const model = key === 'vfg' ? null : (player.interpolationEnabled ? player.interpolationModel : null);
                    setFrameInterpolation(model, next);
                } });
                button.disabled = !ready || player.interpolationChanging; playerPanelContent.append(button);
                const form = element('details', 'enhancement-parameters');
                form.append(element('summary', '', `${title}参数`));
                const select = (key, label, choices) => {
                    const row = element('label', 'enhancement-parameter'); row.append(element('span', '', label));
                    const control = element('select'); control.setAttribute('aria-label', label);
                    for (const [value, text] of choices) { const option = element('option', '', text); option.value = String(value); option.selected = draft[key] === value; control.append(option); }
                    control.value = String(draft[key]); control.addEventListener('change', () => { draft[key] = Number(control.value); }); row.append(control); form.append(row);
                };
                const range = (key, label, min, max) => {
                    const row = element('label', 'enhancement-parameter'); const text = element('span', '', `${label}：${draft[key]}`);
                    const control = element('input'); control.type = 'range'; control.min = String(min); control.max = String(max); control.step = '0.05'; control.value = String(draft[key]); control.setAttribute('aria-label', label);
                    control.addEventListener('input', () => { draft[key] = Number(control.value); text.textContent = `${label}：${draft[key]}`; }); row.append(text, control); form.append(row);
                };
                const toggle = (key, label) => { const row = element('label', 'enhancement-parameter'); const control = element('input'); control.type = 'checkbox'; control.checked = draft[key]; control.addEventListener('change', () => { draft[key] = control.checked; }); row.append(element('span', '', label), control); form.append(row); };
                if (key === 'vsr') {
                    select('vsrQuality', '超分质量', [[1, '1 · Low'], [2, '2 · Medium'], [3, '3 · High'], [4, '4 · Ultra']]);
                    select('vsrTarget', '目标尺寸（保持比例）', [[0, '原尺寸的 2 倍'], [1440, 'QHD · 1440p'], [2160, '4K · 2160p'], [2880, '5K · 2880p'], [3456, '6K · 3456p'], [4032, '7K · 4032p'], [4320, '8K · 4320p']]);
                } else if (key === 'vfg') {
                    select('vfgQuality', '插帧质量', [[0, 'Low'], [1, 'Medium'], [2, 'High']]);
                    select('multiplier', '帧率倍数', [2, 3, 4, 5, 6, 7, 8].map((n) => [n, `${n} 倍`])); toggle('sceneDetection', '自动检测镜头切换');
                    form.append(element('p', 'enhancement-help', '高倍率和高质量会增加处理耗时。出现音画不同步时，可降低倍率或质量；播放器不会自动降档。'));
                } else {
                    select('nrStyle', '模型风格', [[0, '风格 0'], [1, '风格 1'], [2, '风格 2']]);
                    select('nrTarget', '处理尺寸上限（保持比例）', [[1080, '实时 · 1080p'], [0, '原始尺寸'], [480, '480p'], [720, '720p'], [900, '900p'], [1440, '1440p']]);
                    range('nrIntensity', '增强强度', 0, 1); range('nrTone', '局部色调', 0, 1); range('nrStructure', '局部结构', 0, 1);
                    select('nrSkin', '皮肤结构', [[-1, '自动'], [0, '0'], [0.5, '0.5'], [1, '1'], [1.5, '1.5'], [2, '2']]); toggle('nrAutoMask', '自动遮罩'); toggle('nrUiCorrection', 'UI 修正');
                }
                const apply = element('button', 'secondary-command', '应用参数并启用'); apply.type = 'button'; apply.disabled = !ready || player.interpolationChanging;
                apply.addEventListener('click', () => setFrameInterpolation(key === 'vfg' ? null : selectedModel, { ...draft, [key]: true })); form.append(apply); playerPanelContent.append(form);
            }
            playerPanelContent.append(element('p', 'player-panel-state', 'VFG 与 RIFE 互斥；增强支持 SDR、HDR10 和 HLG，暂不支持 HDR10+ 和 Dolby Vision。HLG 按 1000 尼特参考处理；RIFE 暂不支持 HLG。参数在当前播放和切集时保留，应用时保持进度与暂停状态。'));
            return;
        }

        byId('player-panel-title').textContent = '播放信息';
        const source = info.sourceVideo || {};
        const codec = [source.codec, source.profile].filter(Boolean).join(' ');
        const size = source.width && source.height ? `${source.width} × ${source.height}` : '';
        const frameRate = Number.isFinite(source.frameRate) ? `${source.frameRate.toFixed(3)} fps` : '';
        const color = [source.colorSpace, source.colorTransfer, source.colorRange].filter(Boolean).join(' · ');
        const luminance = [source.maxCll && `MaxCLL ${source.maxCll}`, source.maxFall && `MaxFALL ${source.maxFall}`].filter(Boolean).join(' · ');
        const delivery = info.deliveryMode === 'direct_cdn' ? 'CDN 直连' : '服务器流';
        const bitrate = Number.isFinite(info.bitrate) ? `${(info.bitrate / 1000000).toFixed(1)} Mbps` : '';
        const video = createInfoSection('视频源');
        appendInfoRow(video.list, '源格式', infoValue(codec));
        appendInfoRow(video.list, '分辨率', infoValue([size, frameRate].filter(Boolean).join(' · ')));
        appendInfoRow(video.list, '动态范围', infoValue(source.dynamicRange));
        appendInfoRow(video.list, '位深', source.bitDepth ? `${source.bitDepth}-bit` : '-');
        appendInfoRow(video.list, '色彩', infoValue(color));
        if (luminance) appendInfoRow(video.list, '峰值亮度', luminance);
        appendInfoRow(video.list, '封装 / 码率', infoValue([info.container?.toUpperCase(), bitrate].filter(Boolean).join(' · ')));
        const network = createInfoSection('网络与传输');
        appendInfoRow(network.list, '传输', delivery);
        appendInfoRow(network.list, 'Range 探测', `${info.probeStatus || '-'} · ${info.acceptsRanges ? '支持字节范围' : '不支持字节范围'}`);
        appendInfoRow(network.list, '重定向', Number.isFinite(info.redirectCount) ? `${info.redirectCount} 次` : '-');
        appendInfoRow(network.list, '解析', Number.isFinite(info.resolveMs) ? `${info.resolveMs} ms` : '-');
        appendInfoRow(network.list, '会话直链', info.reused ? '已复用' : '本次解析');
        appendInfoRow(network.list, '目标主机', infoValue(info.targetHost));
        const interpolation = createInfoSection('画面增强');
        const activeInterpolation = info.frameInterpolation;
        const diagnostics = info.frameInterpolationDiagnostics || {};
        if (activeInterpolation) {
            appendInfoRow(interpolation.list, '模式 / 输出', `${activeInterpolation.mode} · ${activeInterpolation.targetFps} fps`);
            appendInfoRow(interpolation.list, '后端 / 运行库', `${activeInterpolation.backend} · ${activeInterpolation.runtimeVersion}`);
            appendInfoRow(interpolation.list, '模型 / Engine', `${activeInterpolation.model} · ${activeInterpolation.engineKey}`);
            appendInfoRow(interpolation.list, '精度 / Scale', `${activeInterpolation.precision} · ${activeInterpolation.scale}`);
            appendInfoRow(interpolation.list, '硬件解码', infoValue(diagnostics.hwdecCurrent || activeInterpolation.hwdec));
        } else {
            appendInfoRow(interpolation.list, 'RIFE 插帧', '关闭');
        }
        const enhancement = videoEnhancementSelection(player.videoEnhancement);
        appendInfoRow(interpolation.list, 'RTX Video 超分', enhancement.vsr ? `已选择 · 质量 ${enhancement.vsrQuality} · ${enhancement.vsrTarget ? `${enhancement.vsrTarget}p` : '2 倍尺寸'}` : '关闭');
        appendInfoRow(interpolation.list, 'NVIDIA VFG 插帧', enhancement.vfg ? `已选择 · ${enhancement.multiplier} 倍帧率 · ${['Low', 'Medium', 'High'][enhancement.vfgQuality]}` : '关闭');
        appendInfoRow(interpolation.list, 'DLSSNR', enhancement.nr ? `已选择 · 风格 ${enhancement.nrStyle} · 强度 ${enhancement.nrIntensity}` : '关闭');
        appendInfoRow(interpolation.list, '容器 / VF / 显示', [
            diagnostics.containerFps,
            diagnostics.estimatedVfFps,
            diagnostics.displayFps,
        ].map((value) => Number.isFinite(value) ? `${Number(value).toFixed(3)}` : '-').join(' / '));
        appendInfoRow(interpolation.list, '输出丢帧', infoValue(diagnostics.frameDropCount));
        appendInfoRow(interpolation.list, '解码丢帧', infoValue(diagnostics.decoderFrameDropCount));
        appendInfoRow(interpolation.list, '时序异常', infoValue(diagnostics.mistimedFrameCount));
        appendInfoRow(interpolation.list, 'VO 延迟', infoValue(diagnostics.voDelayedFrameCount));
        playerPanelContent.append(video.section, network.section, interpolation.section);
    }

    function positionPlayerPanel() {
        if (playerPanel.classList.contains('hidden') || !playerPanelTrigger?.isConnected) return;
        const playerRect = playerView.getBoundingClientRect();
        const triggerRect = playerPanelTrigger.getBoundingClientRect();
        const panelRect = playerPanel.getBoundingClientRect();
        const safeInset = window.innerWidth <= 760 ? 18 : 30;
        const viewportLeft = Math.max(safeInset, playerRect.right - safeInset - panelRect.width);
        playerPanel.style.left = `${Math.round(viewportLeft - playerRect.left)}px`;
        playerPanel.style.bottom = `${Math.round(playerRect.bottom - triggerRect.top + 12)}px`;
    }

    function presentPlayerPanel(activePlayer, kind, trigger) {
        if (player !== activePlayer || activePlayer.exiting) return;
        activePlayer.panelKind = kind;
        playerPanelTrigger = trigger || null;
        window.clearTimeout(controlsTimer);
        playerControls.classList.add('visible');
        playerControls.inert = false;
        activePlayer.exitArmed = false;
        renderPlayerPanel();
        playerPanel.classList.remove('hidden');
        positionPlayerPanel();
        refreshPlayerTools();
        window.setTimeout(() => {
            const selected = kind === 'subtitles'
                ? playerPanelContent.querySelector('.player-subtitle-style-reset')
                : playerPanelContent.querySelector('[aria-checked="true"]');
            if (selected) selected.focus();
            else if (playerPanelTrigger?.isConnected) playerPanelTrigger.focus();
        }, 0);
    }

    function loadPlayerEpisodes(activePlayer) {
        const seriesId = activePlayer.card.seriesId;
        if (!seriesId || Array.isArray(activePlayer.episodes)) {
            return Promise.resolve(Array.isArray(activePlayer.episodes) ? activePlayer.episodes : []);
        }
        if (activePlayer.episodesLoading && activePlayer.episodesLoadPromise) {
            return activePlayer.episodesLoadPromise;
        }
        activePlayer.episodesLoading = true;
        activePlayer.episodesError = '';
        refreshPlayerTools();
        renderPlayerPanel();
        const request = (async () => {
            try {
                const detail = await nativeRequest(
                    'mediaStationCatalog',
                    'series_episodes',
                    ['series_episodes', JSON.stringify({ seriesId })],
                    30000,
                );
                if (player !== activePlayer || activePlayer.exiting || activePlayer.card.seriesId !== seriesId) return [];
                activePlayer.episodes = Array.isArray(detail.episodes) ? detail.episodes : [];
                activePlayer.episodeSeason = episodeSeasonNumber(activePlayer.card);
                return activePlayer.episodes;
            } catch (error) {
                if (player !== activePlayer || activePlayer.exiting) return [];
                activePlayer.episodesError = `选集加载失败：${friendlyError(error)}`;
                showToast(activePlayer.episodesError);
                return [];
            } finally {
                if (player === activePlayer) {
                    activePlayer.episodesLoading = false;
                    activePlayer.episodesLoadPromise = null;
                    refreshPlayerTools();
                    renderPlayerPanel();
                    if (activePlayer.panelKind === 'episodes') {
                        requestAnimationFrame(() => focusElement(playerPanelContent.querySelector('[aria-checked="true"], .track-option, .player-episode-option, .player-season-tab')));
                    }
                }
            }
        })();
        activePlayer.episodesLoadPromise = request;
        return request;
    }

    async function openPlayerPanel(kind, trigger) {
        const panelTrigger = trigger?.closest('.player-more-menu') ? byId('player-more') : trigger;
        closePlayerMore(false);
        const activePlayer = player;
        if (!activePlayer?.loadInfo || activePlayer.trackChanging || activePlayer.trackRefreshing || activePlayer.exiting) return;
        if (activePlayer.panelKind === kind && !playerPanel.classList.contains('hidden')) {
            closePlayerPanel(true);
            return;
        }

        if (kind === 'episodes') {
            presentPlayerPanel(activePlayer, kind, panelTrigger);
            await loadPlayerEpisodes(activePlayer);
            return;
        }
        if (kind === 'audio' || kind === 'subtitles') {
            activePlayer.trackRefreshing = true;
            try {
                const tracks = await nativeRequest(
                    'mediaStationTracks',
                    'tracks',
                    [activePlayer.card.id],
                    15000,
                );
                if (player !== activePlayer || activePlayer.exiting) return;
                applyPlaybackMetadataRefresh(activePlayer, tracks);
            } catch (error) {
                if (player === activePlayer && !activePlayer.exiting) showToast(friendlyError(error));
                return;
            } finally {
                if (player === activePlayer) activePlayer.trackRefreshing = false;
            }
        }
        if (kind === 'info') {
            try {
                const diagnostics = await nativeRequest(
                    'mediaStationFrameInterpolation',
                    'frame_interpolation_diagnostics',
                    ['frame_interpolation_diagnostics', activePlayer.card.id],
                    15000,
                );
                if (player !== activePlayer || activePlayer.exiting) return;
                activePlayer.loadInfo.frameInterpolation = diagnostics.active || null;
                activePlayer.loadInfo.frameInterpolationDiagnostics = diagnostics.playback || {};
                updateFrameInterpolationStatus(diagnostics.status);
            } catch (error) {
                console.error(`RTX frame interpolation diagnostics failed: ${friendlyError(error)}`);
                if (!activePlayer.exiting) showToast(friendlyError(error));
            }
        }
        presentPlayerPanel(activePlayer, kind, panelTrigger);
    }

    function closePlayerPanel(restoreFocus = true) {
        if (!player?.panelKind && playerPanel.classList.contains('hidden')) return;
        playerPanel.classList.add('hidden');
        if (player) player.panelKind = '';
        refreshPlayerTools();
        if (restoreFocus && playerPanelTrigger?.isConnected) playerPanelTrigger.focus();
        playerPanelTrigger = null;
        scheduleControlsHide();
    }

    async function switchPlayerEpisode(episode, { automatic = false, trigger = 'manual' } = {}) {
        const activePlayer = player;
        if (!activePlayer
            || activePlayer.exiting
            || activePlayer.trackChanging
            || activePlayer.episodeChanging
            || activePlayer.autoAdvancePending
            || !episode?.playable) return false;
        if (activePlayer.card.id === episode.id) {
            closePlayerPanel(true);
            return false;
        }
        const previousPlaybackRate = playerPlaybackRate;
        const previous = {
            card: activePlayer.card,
            loadInfo: activePlayer.loadInfo,
            positionMs: activePlayer.positionMs,
            durationMs: activePlayer.durationMs,
            started: activePlayer.started,
            playing: activePlayer.playing,
            buffering: activePlayer.buffering,
            introSkipHandled: activePlayer.introSkipHandled,
            outroSkipHandled: activePlayer.outroSkipHandled,
            automationDisabled: activePlayer.automationDisabled,
            automationWarningShown: activePlayer.automationWarningShown,
            playbackFinishedDuringAdvance: activePlayer.playbackFinishedDuringAdvance,
        };
        const interpolationModel = activePlayer.interpolationModel
            || activePlayer.loadInfo?.frameInterpolation?.modelId
            || preferredInterpolationModel;
        const interpolationMode = activePlayer.interpolationEnabled ? '2x' : 'off';
        activePlayer.episodeChanging = true;
        activePlayer.episodeTarget = episode;
        activePlayer.episodeResumePlaying = automatic || activePlayer.playing;
        activePlayer.started = false;
        activePlayer.buffering = false;
        activePlayer.playing = activePlayer.episodeResumePlaying;
        activePlayer.card = episode;
        activePlayer.positionMs = automatic ? 0 : Math.max(0, Math.round(episode.resumePositionMs || 0));
        activePlayer.durationMs = episode.durationMs || 0;
        activePlayer.episodeSeason = episodeSeasonNumber(episode);
        activePlayer.introSkipHandled = activePlayer.seriesPlaybackSetting.introSkipSeconds === 0
            || activePlayer.positionMs >= activePlayer.seriesPlaybackSetting.introSkipSeconds * 1000;
        activePlayer.outroSkipHandled = false;
        activePlayer.playbackFinishedDuringAdvance = false;
        activePlayer.automationDisabled = false;
        activePlayer.automationWarningShown = false;
        const targetPlaybackRate = automatic ? previousPlaybackRate : 1;
        applyPlayerPlaybackRate(targetPlaybackRate);
        byId('player-title').textContent = cardTitle(episode, true);
        byId('player-subtitle').textContent = [episodePosition(episode), episode.title].filter(Boolean).join(' · ');
        byId('player-loading-title').textContent = cardTitle(episode, true);
        byId('player-loading-meta').textContent = [episodePosition(episode),
            activePlayer.positionMs > 0 ? '从 ' + formatTime(activePlayer.positionMs) + ' 继续' : ''].filter(Boolean).join(' · ');
        closePlayerMore(false);
        setPlayerPoster(episode);
        updatePlayerProgress();
        closePlayerPanel(false);
        setPlayerLoading(true, playerTransitionLoadingLabel(activePlayer));
        refreshPlayerTools();
        if (previous.playing && window.jmpNative) window.jmpNative.playerPause();
        try {
            await waitForPlayerPaint();
            const loadInfo = await nativeRequest(
                'mediaStationLoad',
                'load',
                [
                    episode.id,
                    activePlayer.positionMs,
                    interpolationMode,
                    interpolationModel,
                    playbackPreferenceScope(episode),
                    playerSubtitleStyle.fontSize,
                    playerSubtitlePosition(),
                    '',
                    JSON.stringify(activePlayer.videoEnhancement || { vsr: false, vfg: false }),
                ],
                60000,
            );
            if (player !== activePlayer || activePlayer.exiting) return false;
            activePlayer.loadInfo = loadInfo;
            schedulePlaybackMetadataRefresh(activePlayer);
            applyPlayerPlaybackRate(targetPlaybackRate);
            refreshPlayerTools();
            return true;
        } catch (error) {
            if (player !== activePlayer || activePlayer.exiting) return false;
            activePlayer.episodeChanging = false;
            activePlayer.episodeTarget = null;
            Object.assign(activePlayer, previous);
            if (playerPlaybackRate !== previousPlaybackRate) applyPlayerPlaybackRate(previousPlaybackRate);
            byId('player-title').textContent = cardTitle(previous.card, true);
            byId('player-subtitle').textContent = [episodePosition(previous.card), previous.card.title].filter(Boolean).join(' · ');
            byId('player-loading-title').textContent = cardTitle(previous.card, true);
            setPlayerPoster(previous.card);
            updatePlayerProgress();
            setPlayerLoading(!previous.started || previous.buffering, previous.buffering ? '正在缓冲' : '正在准备播放');
            refreshPlayerTools();
            if (previous.playing && window.jmpNative) window.jmpNative.playerPlay();
            showToast(`剧集切换失败：${friendlyError(error)}`);
            if (automatic && (trigger === 'finished' || previous.playbackFinishedDuringAdvance)) finishPlayer();
            return false;
        }
    }

    async function selectPlayerTrack(kind, key) {
        const activePlayer = player;
        if (!activePlayer || activePlayer.trackChanging || activePlayer.exiting) return;
        activePlayer.trackChanging = true;
        refreshPlayerTools();
        playerPanelContent.querySelectorAll('button').forEach((button) => { button.disabled = true; });
        try {
            const result = await nativeRequest(
                'mediaStationSelectTrack',
                'track_selection',
                [activePlayer.card.id, kind, key],
                45000,
            );
            if (player !== activePlayer || activePlayer.exiting) return;
            if (kind === 'audio') activePlayer.loadInfo.audioTrackKey = result.audioTrackKey;
            else {
                activePlayer.loadInfo.subtitleEnabled = result.subtitleEnabled === true;
                activePlayer.loadInfo.subtitleTrackKey = result.subtitleTrackKey || null;
            }
            renderPlayerPanel();
            refreshPlayerTools();
            requestAnimationFrame(() => {
                focusElement(playerPanelContent.querySelector('[aria-checked="true"]'));
            });
        } catch (error) {
            if (player === activePlayer && !activePlayer.exiting) {
                showToast(friendlyError(error));
                renderPlayerPanel();
            }
        } finally {
            if (player === activePlayer) {
                activePlayer.trackChanging = false;
                refreshPlayerTools();
            }
        }
    }

    function movePlayerPanelFocus(delta) {
        const options = [...playerPanelContent.querySelectorAll('.player-panel-control, .track-option, .player-episode-option')]
            .filter((option) => !option.disabled);
        if (!options.length) {
            playerPanelContent.scrollBy({ top: delta * 88, behavior: 'smooth' });
            return;
        }
        const current = options.indexOf(document.activeElement);
        if (delta < 0 && current <= 0) {
            const selectedSeason = playerPanelContent.querySelector('.player-season-tab[aria-selected="true"]');
            if (selectedSeason) {
                selectedSeason.focus();
                return;
            }
        }
        const next = options[Math.max(0, Math.min(options.length - 1, current < 0 ? 0 : current + delta))];
        if (next) {
            next.scrollIntoView({ block: 'nearest' });
            focusElement(next);
        }
    }

    function movePlayerEpisodeFocus(key) {
        const options = [...playerPanelContent.querySelectorAll('.player-episode-option')]
            .filter((option) => !option.disabled);
        const current = options.indexOf(document.activeElement);
        if (current < 0) return;

        const columns = 1;
        const delta = key === 'ArrowLeft' ? -1
            : key === 'ArrowRight' ? 1
                : key === 'ArrowUp' ? -columns
                    : columns;
        const targetIndex = current + delta;
        const crossesRowBoundary = (key === 'ArrowLeft' && current % columns === 0)
            || (key === 'ArrowRight' && current % columns === columns - 1);
        const target = !crossesRowBoundary ? options[targetIndex] : null;
        if (target) {
            target.scrollIntoView({ block: 'nearest', inline: 'nearest' });
            focusElement(target);
            return;
        }

        if (key === 'ArrowUp' && current < columns) {
            const controls = [...playerPanelContent.querySelectorAll('.player-panel-control')]
                .filter((option) => !option.disabled);
            const previous = controls.at(-1) || playerPanelContent.querySelector('.player-season-tab[aria-selected="true"]');
            if (previous) focusElement(previous);
        }
    }

    function requestPlayerExit() {
        const activePlayer = player;
        if (!activePlayer || activePlayer.exiting) return;
        if (!window.jmpNative) {
            showToast('播放器服务不可用，无法退出播放');
            return;
        }
        activePlayer.exiting = true;
        activePlayer.exitArmed = false;
        activePlayer.scrubbing = false;
        activePlayer.scrubPositionMs = null;
        window.clearTimeout(playerClickTimer);
        playerClickTimer = 0;
        playerClickAt = 0;
        window.clearTimeout(controlsTimer);
        closePlayerPanel(false);
        hidePlayerFeedback();
        setPlayerLoading(true, '正在退出播放');
        playerControls.classList.add('visible');
        playerControls.inert = false;
        refreshPlayerTools();
        refreshPlayerCursor();
        if (activePlayer.started) sendPlayerStop(activePlayer);
    }

    function sendPlayerStop(activePlayer) {
        if (player !== activePlayer || !activePlayer.exiting || activePlayer.stopSent) return;
        activePlayer.stopSent = true;
        try {
            // MPV reports END_FILE only after unloading the file and its filter
            // chain. Keep the player layer active until that terminal event so
            // RIFE/D3D11 teardown cannot overlap the resumed home animations.
            window.jmpNative.playerStop();
        } catch (error) {
            if (player !== activePlayer) return;
            activePlayer.stopSent = false;
            activePlayer.exiting = false;
            const loading = !activePlayer.started || activePlayer.interpolationChanging || activePlayer.episodeChanging || activePlayer.buffering;
            const label = activePlayer.interpolationChanging || activePlayer.episodeChanging
                ? playerTransitionLoadingLabel(activePlayer)
                : (activePlayer.buffering ? '正在缓冲' : '正在准备播放');
            setPlayerLoading(loading, label);
            refreshPlayerTools();
            showToast(`退出播放失败：${friendlyError(error)}`);
        }
    }

    function finishPlayer() {
        // Update Continue Watching from the player state before returning to
        // the app. Invalidate any older home request so it cannot overwrite
        // this newer local state while the stopped report is still pending.
        const activeCardId = player?.card?.id || '';
        danmakuRenderer.clear();
        const resumeCardRects = player?.resumeCardRects || new Map();
        if (player && player.positionMs > 0 && homeData && Array.isArray(homeData.resume)) {
            const id = player.card?.id;
            const index = id ? homeData.resume.findIndex((item) => item.id === id) : -1;
            if (id) {
                const item = index >= 0 ? homeData.resume[index] : { ...player.card };
                item.resumePositionMs = Math.max(player.positionMs, item.resumePositionMs || 0);
                if (player.durationMs > 0) {
                    item.durationMs = Math.max(player.durationMs, item.durationMs || 0);
                }
                if (index >= 0) homeData.resume.splice(index, 1);
                homeData.resume.unshift(item);
                homeData.resume.splice(maximumContinueWatchingItems);
                invalidateHomeRefresh();
            }
        }
        window.clearTimeout(playerClickTimer);
        playerClickTimer = 0;
        playerClickAt = 0;
        closePlayerPanel(false);
        player = null;
        playerView.classList.remove('preparing');
        const posterImg = byId('player-poster').firstElementChild;
        posterImg.removeAttribute('src');
        posterImg.classList.remove('image-ready', 'image-error');
        updatePlayerProgress();
        refreshPlayerTools();
        window.clearTimeout(controlsTimer);
        hidePlayerFeedback();
        playerView.classList.add('hidden');
        heroCarouselController?.resume('player');
        refreshPlayerCursor();
        setPlayerMode(false);
        appShell.classList.remove('hidden');
        window.requestAnimationFrame(refreshMediaRowCarousels);
        // Re-render immediately from the player state. The native
        // "home_stale" event is emitted only after the stopped report succeeds;
        // that later refresh verifies and reconciles the local update.
        if (currentView?.kind === 'home' && homeData) {
            const saved = captureView();
            currentView = { ...currentView, data: homeData };
            renderHome(homeData);
            if (saved) restoreViewState(saved, { restoreFocus: Boolean(saved.focusKey) });
            animateMediaRowReorder('resume', resumeCardRects, activeCardId);
        }
        if (homeVerificationPending) {
            homeVerificationPending = false;
            scheduleHomeRefresh(200);
        }
        if (playerControls.contains(document.activeElement)) document.activeElement.blur();
    }

    function showPlayerControls(focusTarget = '') {
        playerControls.classList.add('visible');
        playerControls.inert = false;
        if (player) player.exitArmed = focusTarget === 'exit';
        if (focusTarget === 'exit') focusElement(byId('player-exit'));
        refreshPlayerCursor();
        scheduleControlsHide();
    }

    function isDanmakuConfigurationActive() {
        const control = byId('player-danmaku-control');
        return control.matches(':hover') || Boolean(control.querySelector(':focus-visible'));
    }

    function hidePlayerControls() {
        if (player?.panelKind || player?.buffering || !byId('player-more-menu').classList.contains('hidden')
            || isDanmakuConfigurationActive() || playerControls.querySelector(':focus-visible')) return;
        playerControls.classList.remove('visible');
        playerControls.inert = true;
        if (player) player.exitArmed = false;
        if (playerControls.contains(document.activeElement)) document.activeElement.blur();
        refreshPlayerCursor();
    }

    function refreshPlayerCursor() {
        const shouldHide = Boolean(player && window._isFullscreen === true && !playerControls.classList.contains('visible'));
        playerView.classList.toggle('cursor-hidden', shouldHide);
    }

    function showPlayerFeedback(value, durationMs = 2000, onElapsed = null) {
        window.clearTimeout(feedbackTimer);
        const feedback = byId('player-feedback');
        const body = feedback.querySelector('.player-feedback-body');
        const icon = typeof value === 'object' && value ? value.icon || '' : '';
        body.dataset.icon = icon;
        body.querySelector('.player-feedback-text').textContent = icon ? '' : String(value ?? '');
        feedback.classList.remove('hidden');
        if (durationMs > 0) {
            feedbackTimer = window.setTimeout(() => {
                feedbackTimer = 0;
                if (onElapsed) onElapsed();
                else hidePlayerFeedback();
            }, durationMs);
        }
    }

    function hidePlayerFeedback() {
        window.clearTimeout(feedbackTimer);
        feedbackTimer = 0;
        byId('player-feedback').classList.add('hidden');
    }

    function scheduleControlsHide() {
        window.clearTimeout(controlsTimer);
        if (!player?.playing || !player.started || player.panelKind || player.buffering || player.exiting || !byId('player-more-menu').classList.contains('hidden') || isDanmakuConfigurationActive()) return;
        controlsTimer = window.setTimeout(hidePlayerControls, 3000);
    }

    function formatDuration(ms) {
        if (!ms) return '';
        const minutes = Math.round(ms / 60000);
        return minutes >= 60 ? `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分` : `${minutes} 分钟`;
    }

    function formatTime(ms) {
        const total = Math.max(0, Math.floor((ms || 0) / 1000));
        const hours = Math.floor(total / 3600);
        const minutes = Math.floor(total % 3600 / 60);
        const seconds = total % 60;
        return hours > 0
            ? `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`
            : `${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
    }

    const narrowSidebar = window.matchMedia('(max-width: 480px)');
    function updateSidebarToggle() {
        const expanded = narrowSidebar.matches ? appShell.classList.contains('sidebar-open') : !appShell.classList.contains('sidebar-collapsed');
        byId('sidebar-toggle').setAttribute('aria-expanded', String(expanded));
        byId('sidebar-toggle').title = expanded ? '收起侧栏' : '展开侧栏';
        byId('sidebar-toggle').setAttribute('aria-label', byId('sidebar-toggle').title);
    }
    byId('sidebar-toggle').addEventListener('click', () => {
        appShell.classList.toggle(narrowSidebar.matches ? 'sidebar-open' : 'sidebar-collapsed');
        updateSidebarToggle();
        requestAnimationFrame(refreshHeroArtworkQualification);
    });
    narrowSidebar.addEventListener('change', updateSidebarToggle);
    updateSidebarToggle();
    byId('theme-toggle').addEventListener('click', () => applyUiTheme(document.documentElement.dataset.uiTheme === 'dark' ? 'light' : 'dark', true));
    function closePlayerMore(restoreFocus = false) {
        const menu = byId('player-more-menu'); const wasOpen = !menu.classList.contains('hidden');
        menu.classList.add('hidden'); byId('player-more').setAttribute('aria-expanded', 'false');
        if (wasOpen && restoreFocus) byId('player-more').focus(); scheduleControlsHide();
    }
    byId('player-more').addEventListener('click', (event) => {
        if (!player || player.exiting) return;
        const menu = byId('player-more-menu'); const opening = menu.classList.contains('hidden');
        closePlayerPanel(false); menu.classList.toggle('hidden', !opening);
        byId('player-more').setAttribute('aria-expanded', String(opening)); showPlayerControls();
        if (opening && event.detail === 0) menu.querySelector('button:not(:disabled):not(.hidden)')?.focus();
    });
    byId('player-seek-back').addEventListener('click', () => { seekBy(-10000); showPlayerFeedback('−10 秒', 750); showPlayerControls(); });
    byId('player-seek-forward').addEventListener('click', () => { seekBy(10000); showPlayerFeedback('+10 秒', 750); showPlayerControls(); });
    byId('player-next').addEventListener('click', async () => {
        const activePlayer = player; if (!activePlayer?.started || activePlayer.exiting) return;
        await loadPlayerEpisodes(activePlayer); if (player !== activePlayer || activePlayer.exiting) return;
        const next = nextPlayerEpisode(activePlayer);
        if (next) await switchPlayerEpisode(next); else if (!activePlayer.episodesError) showPlayerFeedback('已到最后一集');
    });
    playerView.addEventListener('click', (event) => { if (!event.target.closest('.player-more-control')) closePlayerMore(false); });
    playerControls.addEventListener('focusin', () => showPlayerControls());
    playerControls.addEventListener('focusout', () => scheduleControlsHide());
    byId('brand-home').addEventListener('click', goHome);
    byId('content-to-top').addEventListener('click', () => smoothScrollTo(content, { top: 0 }));
    byId('nav-home').addEventListener('click', goHome);
    byId('nav-library').addEventListener('click', () => homeData && setCurrentView({ kind: 'libraries', data: homeData.libraries }));
    byId('nav-resume').addEventListener('click', () => homeData && setCurrentView({ kind: 'resume', data: homeData }));
    byId('nav-favorites').addEventListener('click', () => {
        const data = { items: [], totalRecordCount: 0, loading: false, error: '' };
        setCurrentView({ kind: 'favorites', data });
    });
    byId('player-failure-back').addEventListener('click', () => {
        const card = playbackFailure?.card; dismissPlaybackFailure();
        if (card && currentView?.kind !== 'detail') void openDetail(card);
    });
    byId('player-retry').addEventListener('click', () => {
        const failed = playbackFailure;
        if (failed) void startPlayback(failed.card, failed.positionMs, failed.mediaSourceId);
    });
    byId('settings-reduce-motion').addEventListener('change', (event) => applyMotionPreference(event.currentTarget.checked, true));
    try { applyMotionPreference(window.localStorage.getItem('MediaStationGo.Windows.reduceMotion.v1') === 'true'); }
    catch (error) { console.error('读取动态效果设置失败：', error); }
    byId('player-panel-close').replaceChildren(strokeIcon('close'));
    byId('settings-open').querySelector('svg').replaceWith(strokeIcon('settings'));
    for (const [section, icon] of [['appearance', 'appearance'], ['playback', 'play'], ['network', 'network'], ['update', 'update']]) {
        const button = document.querySelector(`.settings-nav [data-settings-section="${section}"]`);
        button.querySelector('span').remove(); button.prepend(strokeIcon(icon));
    }
    for (const [id, variants] of [
        ['player-playback', [['play', 'play'], ['pause', 'pause']]],
        ['player-volume-toggle', [['volume', 'volume'], ['muted', 'muted']]],
        ['player-fullscreen', [['fullscreen', 'enter-fullscreen'], ['restore', 'exit-fullscreen']]],
    ]) {
        byId(id).replaceChildren(...variants.map(([name, state]) => {
            const icon = playerIcon(name); icon.classList.add('player-state-icon', `player-icon-${state}`); return icon;
        }));
    }
    byId('player-panel-close').addEventListener('click', () => closePlayerPanel(true));
    byId('password-visibility').append(strokeIcon('eye'));
    byId('password-visibility').addEventListener('click', () => {
        const visible = byId('password').type === 'password';
        byId('password').type = visible ? 'text' : 'password';
        byId('password-visibility').setAttribute('aria-label', visible ? '隐藏密码' : '显示密码');
        byId('password-visibility').setAttribute('aria-pressed', String(visible));
    });
    for (const [id, icon, label] of [
        ['player-seek-back', 'back', ''], ['player-seek-forward', 'forward', ''],
        ['player-episodes', 'list', '选集'], ['player-speed', 'speed', '倍速'],
        ['player-subtitles', 'subtitles', '字幕'], ['player-audio', 'audio', '音轨'],
        ['player-more', 'more', ''],
    ]) {
        const button = byId(id); button.replaceChildren(playerIcon(icon));
        if (label) button.append(element('span', 'player-tool-label', label));
    }
    [['player-interpolation', 'interpolation', '画面增强'], ['player-danmaku', 'danmaku', '弹幕'], ['player-info', 'info', '播放信息']].forEach(([id, icon, label]) => byId(id).replaceChildren(playerIcon(icon), element('span', 'player-tool-label', label)));
    byId('topbar-search').addEventListener('submit', (event) => {
        event.preventDefault();
        const query = byId('search-open').value;
        const data = { query: '', draft: query, items: null, status: 'idle', error: '' };
        setCurrentView({ kind: 'search', data }); void submitSearch(data, query);
    });
    byId('account-open').addEventListener('click', (event) => {
        openDrawer(byId('account-drawer'), event.currentTarget);
        loadSavedAccounts();
    });
    document.querySelectorAll('[data-component-id]').forEach((card) => {
        const id = card.dataset.componentId;
        const confirmation = card.querySelector('.component-confirm');
        card.querySelectorAll('[data-component-operation]').forEach((button) => button.addEventListener('click', () => {
            const op = button.dataset.componentOperation;
            if (['uninstall', 'restart'].includes(op)) {
                confirmation.dataset.operation = op;
                card.querySelector('.component-confirm-copy').textContent = op === 'restart' ? '将停止播放并重启以完成安装。请先关闭其他播放器实例。' : '下次启动时删除本组件和专属缓存；媒体和其他组件不受影响。';
                confirmation.classList.remove('hidden'); card.querySelector('[data-component-confirm]').focus();
            } else changeVideoComponent(id, op);
        }));
        card.querySelector('[data-component-confirm]').addEventListener('click', () => { const op = confirmation.dataset.operation; confirmation.classList.add('hidden'); changeVideoComponent(id, op); });
        card.querySelector('[data-component-dismiss]').addEventListener('click', () => confirmation.classList.add('hidden'));
    });
    byId('settings-import-frame-interpolation').addEventListener('click', async () => {
        interpolationComponentTracking = true;
        try { await nativeRequest('mediaStationFrameInterpolation', 'video_component_import', ['video_component_import', 'rife'], 15000); }
        catch (error) { showToast(friendlyError(error)); }
        await refreshFrameInterpolationStatus();
    });
    byId('components-open').addEventListener('click', () => setCurrentView({ kind: 'components', data: {} }));
    byId('settings-open').addEventListener('click', (event) => {
        openSettings(event.currentTarget);
    });
    byId('login-settings-open').addEventListener('click', (event) => openSettings(event.currentTarget));
    byId('drawer-scrim').addEventListener('click', () => closeDrawer());
    byId('settings-drawer').addEventListener('click', (event) => {
        if (event.target === event.currentTarget) closeDrawer();
    });
    document.querySelectorAll('.drawer-close').forEach((button) => button.addEventListener('click', () => {
        if (button.closest('.settings-page')) goBack(); else closeDrawer();
    }));
    proxyModeInputs().forEach((input) => input.addEventListener('change', (event) => {
        void updateGlobalProxyMode(event.currentTarget.value);
    }));
    document.querySelectorAll('.settings-nav [data-settings-section]').forEach((button) => {
        button.addEventListener('click', () => selectSettingsSection(button.dataset.settingsSection));
    });
    document.querySelectorAll('[data-ui-theme]').forEach((button) => {
        button.addEventListener('click', () => applyUiTheme(button.dataset.uiTheme, true));
    });
    applyUiTheme(loadUiTheme());
    byId('overview-close').addEventListener('click', closeOverview);
    byId('overview-scrim').addEventListener('click', (event) => {
        if (event.target === event.currentTarget) closeOverview();
    });
    byId('source-close').addEventListener('click', () => closeSourcePicker());
    byId('source-scrim').addEventListener('click', (event) => {
        if (event.target === event.currentTarget) closeSourcePicker();
    });
    byId('change-account-button').addEventListener('click', () => beginAddAccount(session?.baseUrl || ''));
    byId('login-add-account').addEventListener('click', (event) => beginAddAccount('', false, event.currentTarget));
    byId('account-delete-scrim').addEventListener('click', (event) => {
        if (event.target === event.currentTarget) closeDeleteAccountDialog();
    });
    byId('account-delete-close').addEventListener('click', () => closeDeleteAccountDialog());
    byId('account-delete-cancel').addEventListener('click', () => closeDeleteAccountDialog());
    byId('account-delete-confirm').addEventListener('click', confirmDeleteAccount);
    byId('login-cancel').addEventListener('click', cancelAccountChange);
    const defaultSubtitleFontSize = byId('settings-subtitle-font-size');
    const defaultSubtitleBottomOffset = byId('settings-subtitle-bottom-offset');
    const updateDefaultSubtitleStyle = (persist) => {
        applyDefaultPlayerSubtitleStyle({
            fontSize: Number(defaultSubtitleFontSize.value),
            bottomOffset: Number(defaultSubtitleBottomOffset.value),
        }, persist);
    };
    defaultSubtitleFontSize.addEventListener('input', () => updateDefaultSubtitleStyle(false));
    defaultSubtitleFontSize.addEventListener('change', () => updateDefaultSubtitleStyle(true));
    defaultSubtitleBottomOffset.addEventListener('input', () => updateDefaultSubtitleStyle(false));
    defaultSubtitleBottomOffset.addEventListener('change', () => updateDefaultSubtitleStyle(true));
    byId('settings-reset-subtitle-style').addEventListener('click', () => {
        applyDefaultPlayerSubtitleStyle(playerSubtitleStyleDefaults, true);
    });
    refreshDefaultSubtitleStyleControls();
    refreshDanmakuStyleControls();
    // 未开始播放时先下发全局默认和样式；开始播放后继续由全局状态驱动。
    pushDanmakuEnabled();
    pushDanmakuStyle();
    const autoUpdateToggle = byId('settings-auto-update-check');
    autoUpdateToggle.checked = autoUpdateCheckEnabled;
    autoUpdateToggle.addEventListener('change', () => {
        autoUpdateCheckEnabled = autoUpdateToggle.checked;
        if (window.jmpNative?.setSettingValue) {
            window.jmpNative.setSettingValue('advanced', 'autoUpdateCheck', String(autoUpdateCheckEnabled));
        }
        if (autoUpdateCheckEnabled) scheduleAutomaticUpdateCheck();
        else {
            window.clearTimeout(automaticUpdateTimer);
            automaticUpdateTimer = 0;
        }
    });
    byId('settings-check-app-update').addEventListener('click', requestUpdateCheck);
    document.querySelectorAll('input[name="settings-update-source-mode"]').forEach((radio) => {
        radio.addEventListener('change', () => {
            updateDownloadSourceMode = radio.value;
            window.jmpNative?.setSettingValue?.('advanced', 'updateDownloadSourceMode', updateDownloadSourceMode);
            refreshUpdateDownloadSourceControls();
            const status = byId('settings-update-download-sources-status');
            if (status) {
                status.textContent = updateDownloadSourceMode === 'server'
                    ? '已切换为服务器策略。'
                    : updateDownloadSourceMode === 'builtin' ? '已切换为内置默认源。' : '已切换为本地自定义列表。';
                status.dataset.state = 'ready';
            }
        });
    });
    byId('settings-save-update-download-sources').addEventListener('click', saveUpdateDownloadSources);
    byId('settings-reset-update-download-sources').addEventListener('click', resetUpdateDownloadSources);
    refreshUpdateDownloadSourceControls();
    byId('settings-download-app-update').addEventListener('click', () => {
        if (!window.jmpNative?.updateDownload) return;
        appUpdateState = { status: 'downloading', payload: appUpdateState.payload };
        refreshUpdateControls();
        window.jmpNative.updateDownload();
    });
    byId('settings-install-app-update').addEventListener('click', () => {
        byId('settings-confirm-app-update').classList.remove('hidden');
        byId('settings-confirm-install-app-update').focus();
    });
    byId('settings-cancel-app-update').addEventListener('click', () => {
        byId('settings-confirm-app-update').classList.add('hidden');
        byId('settings-install-app-update').focus();
    });
    byId('settings-confirm-install-app-update').addEventListener('click', () => {
        if (!window.jmpNative?.updateInstall) return;
        appUpdateState = { status: 'verifying', payload: appUpdateState.payload };
        refreshUpdateControls();
        window.jmpNative.updateInstall();
    });
    byId('settings-install-frame-interpolation').addEventListener('click', () => changeFrameInterpolationComponent('frame_interpolation_component_install'));
    byId('settings-cancel-install-frame-interpolation').addEventListener('click', () => changeFrameInterpolationComponent('frame_interpolation_component_cancel_install'));
    byId('settings-restart-frame-interpolation').addEventListener('click', showFrameInterpolationRestartConfirmation);
    byId('settings-confirm-restart-frame-interpolation').addEventListener('click', restartForFrameInterpolationInstall);
    byId('settings-later-restart-frame-interpolation').addEventListener('click', deferFrameInterpolationRestart);
    byId('settings-uninstall-frame-interpolation').addEventListener('click', () => {
        byId('settings-confirm-frame-interpolation-uninstall').classList.remove('hidden');
        byId('settings-confirm-uninstall-frame-interpolation').focus();
    });
    byId('settings-cancel-uninstall-frame-interpolation').addEventListener('click', () => {
        byId('settings-confirm-frame-interpolation-uninstall').classList.add('hidden');
        byId('settings-uninstall-frame-interpolation').focus();
    });
    byId('settings-confirm-uninstall-frame-interpolation').addEventListener('click', () => changeFrameInterpolationComponent('frame_interpolation_component_uninstall'));
    byId('settings-clear-image-cache').addEventListener('click', async (event) => {
        const button = event.currentTarget;
        button.disabled = true;
        const view = captureView();
        if (view) currentView = view;
        try {
            const stats = await nativeRequest('mediaStationCatalog', 'clear_image_cache', ['clear_image_cache', '{}']);
            imageDiskStats = {
                imageBytes: Number(stats.imageBytes) || 0,
                imageCount: Number(stats.imageCount) || 0,
            };
            imageCache.clear();
            renderCurrentView();
            restoreViewState(currentView);
            updateImageCacheStatus();
            showToast('缓存已清除');
        } catch (error) {
            console.error(`图片缓存清理失败：${friendlyError(error)}`);
            showToast(`缓存清理失败：${friendlyError(error)}`);
        } finally {
            button.disabled = false;
        }
    });
    byId('player-exit').addEventListener('click', requestPlayerExit);
    byId('player-playback').addEventListener('click', togglePlayback);
    byId('player-episodes').addEventListener('click', (event) => openPlayerPanel('episodes', event.currentTarget));
    byId('player-volume-toggle').addEventListener('click', togglePlayerMuted);
    byId('player-speed').addEventListener('click', (event) => openPlayerPanel('speed', event.currentTarget));
    byId('player-danmaku').addEventListener('click', togglePlayerDanmaku);
    const danmakuControl = byId('player-danmaku-control');
    const keepDanmakuControlsVisible = () => {
        window.clearTimeout(controlsTimer);
        playerControls.classList.add('visible');
        playerControls.inert = false;
        refreshPlayerCursor();
        refreshDanmakuStyleControls();
    };
    danmakuControl.addEventListener('pointerenter', keepDanmakuControlsVisible);
    danmakuControl.addEventListener('focusin', keepDanmakuControlsVisible);
    danmakuControl.addEventListener('pointerleave', scheduleControlsHide);
    danmakuControl.addEventListener('focusout', (event) => {
        if (!danmakuControl.contains(event.relatedTarget)) scheduleControlsHide();
    });
    for (const definition of danmakuStyleControlDefinitions) {
        const input = byId(definition.id);
        const update = (persist) => {
            applyDanmakuStyle({ ...danmakuStyle, [definition.property]: Number(input.value) }, persist);
            showPlayerControls();
        };
        input.addEventListener('input', () => update(false));
        input.addEventListener('change', () => update(true));
    }
    byId('player-danmaku-reset').addEventListener('click', () => {
        applyDanmakuStyle(danmakuStyleDefaults, true);
        showPlayerFeedback('弹幕样式已恢复');
        showPlayerControls();
    });
    const playerVolumeControl = byId('player-volume');
    playerVolumeControl.addEventListener('input', (event) => {
        applyPlayerVolume(event.currentTarget.value, false);
        showPlayerControls();
    });
    playerVolumeControl.addEventListener('change', (event) => applyPlayerVolume(event.currentTarget.value, true));
    byId('player-interpolation').addEventListener('click', (event) => openPlayerPanel('interpolation', event.currentTarget));
    byId('player-fullscreen').addEventListener('click', togglePlayerFullscreen);
    const playerProgress = byId('player-progress');
    playerProgress.addEventListener('pointerdown', (event) => previewProgressSeek(event.currentTarget.value));
    playerProgress.addEventListener('input', (event) => previewProgressSeek(event.currentTarget.value));
    playerProgress.addEventListener('change', (event) => commitProgressSeek(event.currentTarget.value));
    playerProgress.addEventListener('pointerup', (event) => {
        const activePlayer = player;
        const value = event.currentTarget.value;
        window.setTimeout(() => {
            if (player === activePlayer && activePlayer?.scrubbing) commitProgressSeek(value);
        }, 0);
    });
    playerProgress.addEventListener('pointercancel', cancelProgressSeek);
    byId('player-subtitles').addEventListener('click', (event) => openPlayerPanel('subtitles', event.currentTarget));
    byId('player-audio').addEventListener('click', (event) => openPlayerPanel('audio', event.currentTarget));
    byId('player-info').addEventListener('click', (event) => openPlayerPanel('info', event.currentTarget));
    playerView.addEventListener('mousemove', () => showPlayerControls(false));
    playerView.addEventListener('click', (event) => {
        if (!player || player.exiting || isPlayerInteractiveTarget(event.target)) return;
        const now = event.timeStamp || performance.now();
        const withinDoubleClickArea = Math.hypot(event.clientX - playerClickX, event.clientY - playerClickY) <= 24;
        const isDoubleClick = playerClickTimer && withinDoubleClickArea && now - playerClickAt <= 420;
        if (isDoubleClick) {
            event.preventDefault();
            window.clearTimeout(playerClickTimer);
            playerClickTimer = 0;
            playerClickAt = 0;
            togglePlayerFullscreen();
            return;
        }
        window.clearTimeout(playerClickTimer);
        playerClickAt = now;
        playerClickX = event.clientX;
        playerClickY = event.clientY;
        playerClickTimer = window.setTimeout(() => {
            playerClickTimer = 0;
            playerClickAt = 0;
            togglePlayback();
        }, 420);
    });

    document.addEventListener('keydown', (event) => {
        if (playbackFailure) {
            if (event.key === 'Escape') { event.preventDefault(); byId('player-failure-back').click(); }
            else if (event.key === 'Tab') {
                event.preventDefault(); (document.activeElement === byId('player-retry') ? byId('player-failure-back') : byId('player-retry')).focus();
            }
            return;
        }
        if (!byId('account-delete-scrim').classList.contains('hidden')) {
            if (event.key === 'Escape') {
                event.preventDefault();
                closeDeleteAccountDialog();
            }
            return;
        }
        const inputActive = ['INPUT', 'TEXTAREA'].includes(document.activeElement?.tagName);
        if (player) {
            if (event.key === 'Tab') {
                showPlayerControls();
                const scope = !playerPanel.classList.contains('hidden') ? playerPanel
                    : !byId('player-more-menu').classList.contains('hidden') ? byId('player-more-menu') : playerControls;
                const focusable = [...scope.querySelectorAll('button:not(:disabled), input:not(:disabled), select:not(:disabled)')]
                    .filter((node) => node.getClientRects().length && !node.closest('[inert]'));
                if (focusable.length) {
                    const n = focusable.indexOf(document.activeElement);
                    if (n < 0 || (!event.shiftKey && n === focusable.length - 1) || (event.shiftKey && n === 0)) {
                        event.preventDefault(); focusable[event.shiftKey ? focusable.length - 1 : 0].focus();
                    }
                }
                return;
            }
            if (player.exiting) {
                if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Enter', ' ', 'Escape', 'Backspace'].includes(event.key)) {
                    event.preventDefault();
                }
                return;
            }
            if (!inputActive && event.key.toLowerCase() === 'd') {
                event.preventDefault();
                togglePlayerDanmaku();
                return;
            }
            if ((event.key === 'Escape' || event.key === 'Backspace') && !byId('player-more-menu').classList.contains('hidden')) {
                event.preventDefault(); closePlayerMore(true); return;
            }
            const active = document.activeElement;
            const toolButtons = [byId('player-playback'), byId('player-seek-back'), byId('player-seek-forward'), byId('player-next'), byId('player-more'), byId('player-volume-toggle'), byId('player-speed'), byId('player-episodes'), byId('player-danmaku'), byId('player-subtitles'), byId('player-audio'), byId('player-interpolation'), byId('player-info'), byId('player-fullscreen')]
                .filter((button) => !button.disabled && button.getClientRects().length > 0);
            if (!playerPanel.classList.contains('hidden')) {
                if (event.key === 'Escape' || event.key === 'Backspace') {
                    event.preventDefault(); closePlayerPanel(true);
                } else if ((event.key === 'Enter' || event.key === ' ') && toolButtons.includes(active)) {
                    event.preventDefault(); active.click();
                } else if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key) && active?.matches('.player-episode-option')) {
                    event.preventDefault();
                    movePlayerEpisodeFocus(event.key);
                } else if ((event.key === 'ArrowLeft' || event.key === 'ArrowRight') && active?.matches('.player-season-tab')) {
                    event.preventDefault();
                    const tabs = [...playerPanelContent.querySelectorAll('.player-season-tab')];
                    const index = tabs.indexOf(active);
                    const next = tabs[Math.max(0, Math.min(tabs.length - 1, index + (event.key === 'ArrowRight' ? 1 : -1)))];
                    if (next && next !== active) { next.focus(); next.click(); }
                } else if (event.key === 'ArrowDown' && active?.matches('.player-season-tab')) {
                    event.preventDefault();
                    focusElement(playerPanelContent.querySelector('.player-episode-option[aria-checked="true"], .player-episode-option'));
                } else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
                    event.preventDefault(); movePlayerPanelFocus(event.key === 'ArrowDown' ? 1 : -1);
                }
                return;
            }
            if (active === playerVolumeControl && (event.key === 'ArrowLeft' || event.key === 'ArrowRight')) {
                event.preventDefault();
                applyPlayerVolume(playerVolume + (event.key === 'ArrowRight' ? 5 : -5), true);
                showPlayerControls();
                return;
            }
            if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                event.preventDefault();
                if (playerControls.contains(active)) active.blur();
                const controlsWereVisible = playerControls.classList.contains('visible');
                seekRepeatCount = event.repeat ? seekRepeatCount + 1 : 0;
                const step = Math.min(60000, 10000 + seekRepeatCount * 5000);
                const direction = event.key === 'ArrowRight' ? 1 : -1;
                seekBy(direction * step);
                if (controlsWereVisible) showPlayerControls();
                showPlayerFeedback(`${direction > 0 ? '+' : '-'}${step / 1000} 秒`, 650, () => {
                    hidePlayerFeedback();
                });
            }
            else if (event.key === 'ArrowDown' && active === byId('player-progress') && toolButtons.length) {
                event.preventDefault(); toolButtons[0].focus();
            }
            else if (event.key === 'ArrowUp' && toolButtons.includes(active)) {
                event.preventDefault(); active.blur();
            }
            else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') { event.preventDefault(); showPlayerControls(); }
            else if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                if (active === playerVolumeControl) return;
                if (active === byId('player-exit') || toolButtons.includes(active)) active.click();
                else if (playerControls.classList.contains('visible') && player.exitArmed) requestPlayerExit();
                else togglePlayback();
            }
            else if (event.key === 'Escape' || event.key === 'Backspace') {
                event.preventDefault();
                if (playerControls.classList.contains('visible')) hidePlayerControls();
                else showPlayerControls('exit');
            }
            return;
        }
        if (event.key === 'Escape' || (event.key === 'Backspace' && !inputActive)) {
            event.preventDefault();
            if (!loginView.classList.contains('hidden') && currentDrawer) closeDrawer();
            else if (!loginView.classList.contains('hidden') && loginCanCancel) cancelAccountChange();
            else if (!loginView.classList.contains('hidden')) return;
            else goBack();
            return;
        }
        if (inputActive) {
            if (event.key === 'ArrowDown' && document.activeElement?.matches('.search-field input')) {
                const firstResult = content.querySelector('.search-grid .media-card');
                if (firstResult) {
                    event.preventDefault();
                    focusAndReveal(firstResult, 'center', 'nearest');
                }
            } else if (event.key === 'ArrowUp' && document.activeElement?.matches('.search-field input')) {
                event.preventDefault();
                focusElement(byId('search-open'));
            }
            return;
        }
        if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
        if (document.activeElement?.matches('.back-button') && event.key === 'ArrowDown') {
            event.preventDefault();
            const target = content.querySelector('.search-field input, .library-filter-button[aria-pressed="true"], .detail-actions button, .detail-overview, .grid-view .media-card, .people-row .media-card');
            focusAndReveal(target, 'center', 'nearest');
            return;
        }
        const heroControls = document.activeElement?.closest?.('.hero-actions, .hero-carousel-controls, .detail-actions, .row-carousel-controls');
        if (heroControls) {
            event.preventDefault();
            const buttons = [...heroControls.querySelectorAll('button:not(:disabled)')];
            const index = buttons.indexOf(document.activeElement);
            if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                focusElement(buttons[clamp(index + (event.key === 'ArrowRight' ? 1 : -1), 0, buttons.length - 1)]);
            } else if (event.key === 'ArrowDown') {
                const scopedRow = heroControls.classList.contains('row-carousel-controls')
                    ? heroControls.closest('section')?.querySelector('.media-row .media-card')
                    : content.querySelector('.media-row .media-card');
                focusAndReveal(scopedRow, 'center', 'center');
            }
            return;
        }
        const segmented = document.activeElement?.closest?.('.segmented-control');
        if (segmented) {
            event.preventDefault();
            if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                const buttons = [...segmented.querySelectorAll('button')];
                const index = buttons.indexOf(document.activeElement);
                const next = buttons[Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowRight' ? 1 : -1)))];
                if (next && next !== document.activeElement) {
                    focusAndReveal(next, 'nearest', 'nearest');
                    next.click();
                }
            } else if (event.key === 'ArrowDown') {
                focusAndReveal(content.querySelector('.episode-row .media-card'), 'start', 'center');
            }
            return;
        }
        const filterControls = document.activeElement?.closest?.('.library-filter-controls');
        if (filterControls) {
            event.preventDefault();
            const buttons = [...filterControls.querySelectorAll('button:not([hidden]):not(:disabled)')];
            const index = buttons.indexOf(document.activeElement);
            if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                const next = buttons[clamp(index + (event.key === 'ArrowRight' ? 1 : -1), 0, buttons.length - 1)];
                focusAndReveal(next, 'nearest', 'nearest');
            } else if (event.key === 'ArrowDown') {
                focusAndReveal(content.querySelector('.grid-view .media-card'), 'center', 'nearest');
            } else {
                const groups = [...content.querySelectorAll('.library-filter-controls')];
                const previousGroup = groups[groups.indexOf(filterControls) - 1];
                focusAndReveal(previousGroup?.querySelector('[aria-pressed="true"]') || content.querySelector('.back-button'), 'nearest', 'nearest');
            }
            return;
        }
        const card = document.activeElement?.closest?.('.media-card');
        if (!card) return;
        const grid = card.closest('.grid-view');
        if (grid) {
            event.preventDefault();
            const cards = [...grid.querySelectorAll('.media-card')];
            const index = cards.indexOf(card);
            const firstTop = cards[0]?.offsetTop;
            const wrapIndex = cards.findIndex((candidate) => Math.abs(candidate.offsetTop - firstTop) > 2);
            const columnCount = wrapIndex < 1 ? cards.length : wrapIndex;
            let nextIndex = index;
            if (event.key === 'ArrowLeft') nextIndex -= 1;
            if (event.key === 'ArrowRight') nextIndex += 1;
            if (event.key === 'ArrowUp') nextIndex -= columnCount;
            if (event.key === 'ArrowDown') nextIndex += columnCount;
            if (event.key === 'ArrowUp' && nextIndex < 0) {
                const target = content.querySelector('.search-field input, .library-filter-button[aria-pressed="true"], .back-button');
                focusAndReveal(target, 'nearest', 'nearest');
                return;
            }
            nextIndex = clamp(nextIndex, 0, cards.length - 1);
            focusAndReveal(cards[nextIndex], 'center', 'nearest');
            return;
        }
        const row = card.closest('.media-row');
        if (!row) return;
        event.preventDefault();
        if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
            const cards = [...row.querySelectorAll('.media-card')];
            const next = cards[Math.max(0, Math.min(cards.length - 1, cards.indexOf(card) + (event.key === 'ArrowRight' ? 1 : -1)))];
            focusAndReveal(next);
        } else {
            const rows = [...content.querySelectorAll('.media-row')];
            const rowIndex = rows.indexOf(row) + (event.key === 'ArrowDown' ? 1 : -1);
            const targetRow = rows[rowIndex];
            if (!targetRow) {
                if (event.key === 'ArrowUp') {
                    const target = row.classList.contains('episode-row')
                        ? content.querySelector('.episode-ranges [aria-selected="true"], .season-tabs [aria-selected="true"], .season-picker-button')
                        : row.classList.contains('people-row')
                            ? content.querySelector('.detail-actions button, .detail-overview, .detail-back')
                            : content.querySelector('.hero-actions button');
                    focusElement(target);
                }
                return;
            }
            const index = Number(card.dataset.cardIndex) || 0;
            const cards = [...targetRow.querySelectorAll('.media-card')];
            const next = cards[Math.min(index, cards.length - 1)];
            focusAndReveal(next, 'center', 'center');
        }
    });

    function wheelPixels(value, mode, pageSize) {
        if (!Number.isFinite(value)) return 0;
        if (mode === WheelEvent.DOM_DELTA_LINE) return value * 30;
        if (mode === WheelEvent.DOM_DELTA_PAGE) return value * pageSize;
        return value;
    }

    content.addEventListener('wheel', (event) => {
        const row = event.target.closest?.('.media-row');
        const deltaX = wheelPixels(event.deltaX, event.deltaMode, content.clientWidth);
        const deltaY = wheelPixels(event.deltaY, event.deltaMode, content.clientHeight);
        if (row) {
            // Horizontal media row: keep native behavior, but steer vertical
            // leftover deltas into the row's own horizontal scroll.
            if ((event.shiftKey && deltaY) || (deltaX && Math.abs(deltaX) >= Math.abs(deltaY))) {
                event.preventDefault();
            }
            return;
        }
        // Vertical main-content scrolling is left to the native Chromium
        // engine. Custom rAF smoothing proved unstable in the CEF offscreen
        // render path, so native scrolling with the SmoothScrolling feature
        // is the reliable baseline.
    }, { passive: false });

    const rowScrollIdleTimers = new WeakMap();
    content.addEventListener('scroll', (event) => {
        const row = event.target;
        if (row === content) {
            updateTopbarState();
            return;
        }
        if (!(row instanceof Element) || !row.classList.contains('media-row')) return;
        row.classList.add('is-scrolling');
        window.clearTimeout(rowScrollIdleTimers.get(row));
        rowScrollIdleTimers.set(row, window.setTimeout(() => {
            row.classList.remove('is-scrolling');
            rowScrollIdleTimers.delete(row);
        }, 120));
    }, { capture: true, passive: true });

    window.addEventListener('resize', () => {
        refreshHeroArtworkQualification();
        refreshMediaRowCarousels();
        content.querySelectorAll('.library-filter-group').forEach((group) => group._syncOverflow?.());
        if (currentDrawer === byId('account-drawer')) positionAccountPopover(currentDrawer, drawerTrigger);
        positionPlayerPanel();
    }, { passive: true });

    content.addEventListener('pointerdown', (event) => {
        stopSmoothScroll(content);
        const row = event.target.closest?.('.media-row');
        if (row) stopSmoothScroll(row);
    }, { passive: true });

    document.addEventListener('keyup', (event) => {
        if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') seekRepeatCount = 0;
    });

    refreshPlayerVolume();
    initialize();
})();
