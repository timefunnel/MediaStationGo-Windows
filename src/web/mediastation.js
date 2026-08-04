(() => {
    'use strict';

    const byId = (id) => document.getElementById(id);
    const splash = byId('splash');
    const loginView = byId('login-view');
    const appShell = byId('app-shell');
    const appBackdrop = byId('app-backdrop');
    const content = byId('content');
    const playerView = byId('player-view');
    const playerControls = byId('player-controls');
    const playerPanel = byId('player-panel');
    const playerPanelContent = byId('player-panel-content');
    const toast = byId('toast');
    const pending = new Map();
    const imageCache = new Map();
    const imagePending = new Map();
    const imageQueue = [];
    const libraryCache = new Map();
    const scrollMotions = new WeakMap();
    const history = [];
    const homeRefreshDelayMs = 1200;
    const heroRotationIntervalMs = 9000;
    const libraryPageSize = 48;
    const maximumLibraryCacheEntries = 12;
    const playbackInfoSettingKey = 'MediaStationGo.Windows.playbackInfoEnabled.v1';
    const playSymbol = '\u23f5\ufe0e';
    const pauseSymbol = '\u23f8\ufe0e';
    const interpolationModels = Object.freeze({
        'rife-v4.26': { label: '质量优先', name: 'RIFE v4.26' },
        'rife-v4.26-scale0.5': { label: '均衡优先', name: 'RIFE v4.26 · Scale 0.5' },
        'rife-v4.25-lite': { label: '流畅优先', name: 'RIFE v4.25 Lite' },
    });
    let imageActive = 0;
    let imageDiskStats = { imageBytes: 0, imageCount: 0 };
    let imageStatsTimer = 0;
    let requestSequence = 0;
    let toastTimer = 0;
    let drawerTrigger = null;
    let currentDrawer = null;
    let currentView = null;
    let session = null;
    let homeData = null;
    let heroRevision = 0;
    let heroSlideTimer = 0;
    let heroCopyTimer = 0;
    let lastHeroBackdropSrc = '';
    let controlsTimer = 0;
    let feedbackTimer = 0;
    let playerClickTimer = 0;
    let playerClickAt = 0;
    let playerClickX = 0;
    let playerClickY = 0;
    let seekRepeatCount = 0;
    let player = null;
    let playerPanelTrigger = null;
    let overviewTrigger = null;
    let loginCanCancel = false;
    let loginReturnFocus = null;
    let contentTransitionTimer = 0;
    let homeRefreshTimer = 0;
    let homeRefreshGeneration = 0;
    let homeRefreshFailures = 0;
    let heroRotationTimer = 0;
    let playbackInfoEnabled = loadPlaybackInfoSetting();
    let preferredInterpolationModel = 'rife-v4.26';

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
        if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
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
        if (row?._revealCarouselNode) row._revealCarouselNode(node);
        else if (row) smoothScrollTo(row, { left: revealTarget(row, node, 'x', inline) });
        smoothScrollTo(content, { top: revealTarget(content, node, 'y', block) });
        focusElement(node);
    }

    function showToast(message) {
        window.clearTimeout(toastTimer);
        toast.textContent = message;
        toast.classList.remove('hidden');
        toastTimer = window.setTimeout(() => toast.classList.add('hidden'), 3200);
    }

    function loadPlaybackInfoSetting() {
        try {
            const value = window.localStorage.getItem(playbackInfoSettingKey);
            if (value === null || value === 'true') return true;
            if (value === 'false') return false;
            console.error('MediaStation playback info setting is invalid');
        } catch (error) {
            console.error(`MediaStation playback info setting could not be read: ${error}`);
        }
        return true;
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
            external_subtitle_download_failed: '字幕下载失败',
            unsupported_external_subtitle_format: '暂不支持该字幕格式',
            playback_changed: '当前播放项目已变化',
            playback_unavailable: '当前没有可切换轨道的播放项目',
            authentication_in_progress: '登录请求正在处理中',
            session_changed: '账号状态已变化，请重试',
            account_not_found: '所选账号已不存在，请刷新列表',
            invalid_account_id: '所选账号标识无效',
            saved_account_invalid: '所选账号凭据无效',
            credential_enumerate_failed: '无法读取 Windows 中保存的账号',
            credential_read_failed: '无法读取 Windows 中保存的账号凭据',
            credential_write_failed: '无法安全保存账号凭据',
            credential_delete_failed: '无法删除 Windows 中保存的账号凭据',
            credential_account_invalid: '已保存账号凭据无效',
            credential_account_target_invalid: '已保存账号标识无效',
            credential_account_mismatch: '已保存账号标识与凭据不一致',
            credential_persist_rollback_failed: '账号保存失败，且无法恢复之前的账号状态',
            settings_write_failed: '无法保存当前服务器设置',
            frame_interpolation_runtime_unavailable: 'RIFE 插帧组件不可用',
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
        byId('account-name').textContent = name;
        byId('account-avatar').textContent = initial;
        byId('drawer-avatar').textContent = initial;
        byId('drawer-user').textContent = name;
        byId('drawer-server').textContent = session?.baseUrl || '';
    }

    function setPlayerMode(enabled) {
        document.documentElement.classList.toggle('player-mode', enabled);
        document.body.classList.toggle('player-mode', enabled);
    }

    function showLogin(baseUrl = '', message = '', canCancel = false) {
        loginCanCancel = canCancel;
        splash.classList.add('hidden');
        appShell.classList.add('hidden');
        playerView.classList.add('hidden');
        setPlayerMode(false);
        loginView.classList.remove('hidden');
        byId('server-url').value = baseUrl;
        byId('password').value = '';
        byId('login-cancel').classList.toggle('hidden', !canCancel);
        byId('login-error').textContent = message;
        window.setTimeout(() => byId(baseUrl ? 'username' : 'server-url').focus(), 0);
    }

    async function showApp(account) {
        session = account;
        loginCanCancel = false;
        loginReturnFocus = null;
        updateSessionUi();
        splash.classList.add('hidden');
        loginView.classList.add('hidden');
        appShell.classList.remove('hidden');
        byId('login-cancel').classList.add('hidden');
        refreshImageCacheStatus();
        refreshFrameInterpolationStatus();
        await loadHome(true);
    }

    function resetCatalogState() {
        window.clearTimeout(homeRefreshTimer);
        homeRefreshTimer = 0;
        homeRefreshGeneration += 1;
        stopHeroCarousel();
        window.clearTimeout(imageStatsTimer);
        imageStatsTimer = 0;
        homeData = null;
        currentView = null;
        history.length = 0;
        heroRevision += 1;
        window.clearTimeout(heroSlideTimer);
        heroSlideTimer = 0;
        window.clearTimeout(heroCopyTimer);
        heroCopyTimer = 0;
        stopSmoothScroll(content);
        lastHeroBackdropSrc = '';
        imageCache.clear();
        imagePending.clear();
        libraryCache.clear();
        libraryPageObserver.disconnect();
        while (imageQueue.length) {
            imageQueue.shift().reject(new Error('账号状态已变化'));
        }
        updateImageCacheStatus();
    }

    function beginAccountChange() {
        if (!session) return;
        loginReturnFocus = byId('account-open');
        closeDrawer(false);
        byId('username').value = '';
        showLogin(session.baseUrl || '', '', true);
    }

    function cancelAccountChange() {
        if (!loginCanCancel || !session) return;
        loginCanCancel = false;
        loginView.classList.add('hidden');
        appShell.classList.remove('hidden');
        byId('login-error').textContent = '';
        byId('password').value = '';
        byId('login-cancel').classList.add('hidden');
        const target = loginReturnFocus;
        loginReturnFocus = null;
        requestAnimationFrame(() => focusElement(target || byId('account-open')));
    }

    async function initialize() {
        await new Promise((resolve) => window.setTimeout(resolve, 280));
        try {
            const status = await nativeRequest('mediaStationSessionStatus', 'session_status');
            if (status.configured) {
                await showApp(status);
            } else {
                showLogin(status.baseUrl || '');
            }
        } catch (error) {
            showLogin('', friendlyError(error));
        }
    }

    byId('login-form').addEventListener('submit', async (event) => {
        event.preventDefault();
        const button = byId('login-submit');
        const errorText = byId('login-error');
        button.disabled = true;
        errorText.textContent = '';
        try {
            const status = await nativeRequest('mediaStationAuthenticate', 'authenticate', [
                byId('server-url').value.trim(),
                byId('username').value.trim(),
                byId('password').value,
            ]);
            byId('password').value = '';
            resetCatalogState();
            await showApp(status);
        } catch (error) {
            errorText.textContent = friendlyError(error);
        } finally {
            button.disabled = false;
        }
    });

    function captureView() {
        if (!currentView) return null;
        const rows = {};
        content.querySelectorAll('.media-row[data-row-key]').forEach((row) => {
            rows[row.dataset.rowKey] = row.scrollLeft;
        });
        return { ...currentView, scrollTop: content.scrollTop, rows, focusKey: '' };
    }

    function setCurrentView(view, pushHistory = true) {
        const captured = captureView();
        if (pushHistory && captured) history.push(captured);
        currentView = { ...view, scrollTop: 0, rows: {}, focusKey: '' };
        renderCurrentView(captured ? 'forward' : '');
    }

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

    function restoreScroll(view) {
        requestAnimationFrame(() => {
            content.scrollTop = view.scrollTop || 0;
            content.querySelectorAll('.media-row[data-row-key]').forEach((row) => {
                row.scrollLeft = view.rows?.[row.dataset.rowKey] || 0;
            });
        });
    }

    function goBack() {
        if (!byId('overview-scrim').classList.contains('hidden')) {
            closeOverview();
            return;
        }
        if (currentDrawer) {
            closeDrawer();
            return;
        }
        const previous = history.pop();
        if (!previous) {
            if (currentView?.kind !== 'home' && homeData) setCurrentView({ kind: 'home', data: homeData }, false);
            return;
        }
        currentView = previous;
        renderCurrentView('back');
        restoreScroll(previous);
    }

    function renderCurrentView(transition = '') {
        stopSmoothScroll(content);
        if (currentView?.kind !== 'home') stopHeroCarousel();
        updateNavState();
        switch (currentView?.kind) {
            case 'home': renderHome(currentView.data); break;
            case 'libraries': renderLibraries(currentView.data); break;
            case 'library': renderLibrary(currentView.data); break;
            case 'detail': renderDetail(currentView.data); break;
            case 'search': renderSearch(currentView.data); break;
            default: renderEmpty('没有可显示的内容');
        }
        playContentTransition(transition);
    }

    function updateNavState() {
        byId('nav-home').classList.toggle('active', currentView?.kind === 'home');
        byId('nav-library').classList.toggle('active', ['libraries', 'library'].includes(currentView?.kind));
        byId('topbar-home-shortcut').classList.toggle('hidden', currentView?.kind === 'home');
    }

    function renderLoading() {
        content.replaceChildren();
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
        content.replaceChildren(element('div', 'empty-state', message));
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
                    if (saved) restoreScroll(saved);
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
        if (hero) return 1600;
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

    // Blur the current hero backdrop across the whole app background so the
    // page has the ambient, colored glow Blink achieves with its app-backdrop.
    function setAppBackdrop(src) {
        if (!appBackdrop) return;
        appBackdrop.style.backgroundImage = `url("${src}")`;
        appBackdrop.classList.add('ready');
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

    function requestImage(ref, width) {
        const memoryKey = imageMemoryKey(ref, width);
        const cached = imageCache.get(memoryKey);
        if (cached) return Promise.resolve(cached);
        const active = imagePending.get(memoryKey);
        if (active) return active;
        const request = new Promise((resolve, reject) => {
            imageQueue.push({ ref, width, memoryKey, resolve, reject });
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
            const task = imageQueue.shift();
            imageActive += 1;
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
                    pumpImages();
                    if (imageActive === 0 && imageQueue.length === 0) scheduleImageCacheStatsRefresh();
                });
        }
    }

    function updateImageCacheStatus() {
        const diskSize = formatBytes(imageDiskStats.imageBytes);
        byId('image-cache-status').textContent = `磁盘 ${diskSize} · 本次 ${imageCache.size} 张`;
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
            byId('image-cache-status').textContent = '统计失败';
        }
    }

    function updateFrameInterpolationStatus(status) {
        if (!status) return;
        const label = byId('frame-interpolation-status');
        if (interpolationModels[status.selectedModel]) {
            preferredInterpolationModel = status.selectedModel;
        }
        if (status.componentStatus === 'ready') {
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
            const label = byId('frame-interpolation-status');
            label.textContent = friendlyError(error);
            label.dataset.state = 'error';
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

    function cardSubtitle(card, omitSeriesName = false) {
        if (card.type === 'Episode') {
            return [omitSeriesName ? '' : card.seriesName, episodePosition(card)].filter(Boolean).join(' · ');
        }
        return card.year ? String(card.year) : '';
    }

    function cardTitle(card, preferSeriesName = false) {
        return preferSeriesName && card.type === 'Episode' && card.seriesName ? card.seriesName : card.title;
    }

    function progressPercent(card) {
        if (!card.durationMs || !card.resumePositionMs) return 0;
        return Math.max(0, Math.min(100, card.resumePositionMs / card.durationMs * 100));
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
        button.dataset.focusKey = `media:${card.id}`;
        const art = element('span', 'card-art');
        const fallback = element('span', 'art-fallback', initials(title));
        const img = document.createElement('img');
        img.alt = '';
        img.decoding = 'async';
        art.append(fallback, img);
        const ref = options.landscape ? (card.landscapeImage || card.primaryImage) : (card.primaryImage || card.landscapeImage);
        observeImage(img, ref, imageWidthFor(ref, options.landscape));
        if (options.episodePicker && card.indexNumber) {
            art.append(element('span', 'episode-badge', `第 ${card.indexNumber} 集`));
        }
        const progress = progressPercent(card);
        if (progress > 0) {
            const track = element('span', 'card-progress');
            const value = element('span');
            value.style.width = `${progress}%`;
            track.append(value);
            art.append(track);
        }
        if (isLibrary) {
            const overlay = element('span', 'library-card-overlay');
            overlay.append(element('strong', '', title), element('span', '', '打开媒体库'));
            art.append(overlay);
            button.append(art);
        } else {
            const subtitle = options.episodePicker
                ? formatDuration(card.durationMs)
                : cardSubtitle(card, options.omitSeriesName ?? options.episodeAsSeries);
            button.append(art, element('span', 'card-title', title), element('span', 'card-subtitle', subtitle));
        }
        button.addEventListener('focus', () => options.onFocus?.(card));
        button.addEventListener('click', () => options.onClick?.(card));
        return button;
    }

    function createRowCarousel(row, title) {
        row.classList.add('carousel-row');
        const rowShell = element('div', 'media-row-carousel');
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

        let pageDistance = 0;
        let cardStride = 0;
        let pageIndex = 0;
        let maximumPage = 0;
        let active = false;
        let pendingDirection = 0;
        let generation = 0;
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
            const cardWidth = Math.round(rawWidth * pixelRatio) / pixelRatio;
            const gap = Math.round(rawGap * pixelRatio) / pixelRatio;
            row.style.setProperty('--row-card-width', `${cardWidth}px`);
            row.style.setProperty('--row-gap', `${gap}px`);
            cardStride = cardWidth + gap;
            return Math.max(
                cardStride,
                Math.floor((row.clientWidth * 0.78) / cardStride) * cardStride,
            );
        };
        const alignEndToPage = () => {
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
            pageIndex = clamp(previousIndex, 0, maximumPage);
            row.scrollLeft = pageIndex * pageDistance;
            refreshControls();
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
        row._revealCarouselNode = (node) => {
            const cards = [...row.querySelectorAll('.media-card')];
            const cardIndex = cards.indexOf(node.closest('.media-card'));
            if (cardIndex < 0 || cardStride <= 0) return;
            const cardsPerPage = Math.max(1, Math.round(pageDistance / cardStride));
            moveToPage(Math.floor(cardIndex / cardsPerPage));
        };
        rowShell.append(row, previous, next);
        window.requestAnimationFrame(alignEndToPage);
        return rowShell;
    }

    function createSection(title, cards, options = {}) {
        if (!cards?.length) return null;
        const section = element('section', 'media-section');
        const heading = element('div', 'section-heading');
        heading.append(element('h2', '', title));
        if (options.more) {
            const more = element('button', '', '查看全部');
            more.type = 'button';
            more.addEventListener('click', options.more);
            heading.append(more);
        }
        const row = element('div', `media-row home-media-row${options.landscape ? ' landscape' : ''}`);
        row.dataset.rowKey = options.key || title;
        row.dataset.rowIndex = String(options.rowIndex ?? 0);
        cards.forEach((card, index) => row.append(createMediaCard(card, { ...options, index })));
        section.append(heading, createRowCarousel(row, title));
        return section;
    }

    function updateHero(hero, card) {
        if (!card) return;
        const revision = ++heroRevision;
        const copy = hero.querySelector('.hero-copy');
        const nextCopy = document.createDocumentFragment();
        nextCopy.append(element('h1', '', cardTitle(card, true)));
        const meta = element('p', 'hero-meta');
        [card.year, card.dynamicRange, formatDuration(card.durationMs), card.communityRating ? `★ ${card.communityRating.toFixed(1)}` : '']
            .filter(Boolean).forEach((value) => meta.append(element('span', '', value)));
        nextCopy.append(meta);
        if (card.overview) nextCopy.append(element('p', 'hero-overview', card.overview));
        const actions = element('div', 'hero-actions');
        const primary = element('button', 'primary-command hero-primary');
        primary.type = 'button';
        if (card.type === 'CollectionFolder') {
            primary.append(element('span', '', '→'), element('span', '', '打开媒体库'));
            primary.addEventListener('click', () => openLibrary(card));
        } else if (card.playable) {
            const label = card.resumePositionMs ? `继续播放 ${formatTime(card.resumePositionMs)}` : '开始播放';
            primary.append(element('span', '', '▶'), element('span', '', label));
            primary.addEventListener('click', () => startPlayback(card, card.resumePositionMs));
        } else {
            primary.append(element('span', '', '→'), element('span', '', '查看详情'));
            primary.addEventListener('click', () => openDetail(card));
        }
        actions.append(primary);
        if (card.playable && card.type !== 'CollectionFolder' && card.type !== 'Episode') {
            const detail = element('button', 'secondary-command hero-secondary', '查看详情');
            detail.type = 'button';
            detail.addEventListener('click', () => openDetail(card));
            actions.append(detail);
        }
        nextCopy.append(actions);
        let copyApplied = false;
        const applyCopy = (animate) => {
            if (copyApplied) return;
            copyApplied = true;
            window.clearTimeout(heroCopyTimer);
            copy.classList.remove('hero-copy-enter');
            copy.replaceChildren(nextCopy);
            if (!animate) return;
            void copy.offsetWidth;
            copy.classList.add('hero-copy-enter');
            heroCopyTimer = window.setTimeout(() => {
                copy.classList.remove('hero-copy-enter');
                heroCopyTimer = 0;
            }, 340);
        };
        const ref = card.backdropImage || card.landscapeImage;
        if (!copy.childElementCount || !ref) applyCopy(false);
        if (!ref) return;
        requestImage(ref, imageWidthFor(ref, true, true)).then((src) => {
            if (revision !== heroRevision || !hero.isConnected) return;
            const layers = [...hero.querySelectorAll('.hero-backdrop-layer')];
            if (layers.length !== 2) return;
            const activeIndex = Number(hero.dataset.backdropLayer ?? -1);
            const nextIndex = activeIndex === 0 ? 1 : 0;
            applyCopy(activeIndex >= 0);
            window.clearTimeout(heroSlideTimer);
            heroSlideTimer = 0;
            layers.forEach((layer) => {
                layer.classList.remove('sliding');
                layer.style.transition = 'none';
            });
            layers[nextIndex].style.backgroundImage = `url("${src}")`;
            lastHeroBackdropSrc = src;
            setAppBackdrop(src);
            if (activeIndex < 0) {
                layers[nextIndex].style.transform = 'translateX(0)';
                layers[nextIndex].classList.add('active');
                hero.dataset.backdropLayer = String(nextIndex);
                return;
            }
            layers[activeIndex].style.transform = 'translateX(0)';
            layers[nextIndex].style.transform = 'translateX(100%)';
            void hero.offsetWidth;
            layers[activeIndex].style.transition = '';
            layers[nextIndex].style.transition = '';
            layers[activeIndex].classList.add('sliding');
            layers[nextIndex].classList.add('active', 'sliding');
            layers[activeIndex].style.transform = 'translateX(-100%)';
            layers[nextIndex].style.transform = 'translateX(0)';
            hero.dataset.backdropLayer = String(nextIndex);
            heroSlideTimer = window.setTimeout(() => {
                if (!hero.isConnected || Number(hero.dataset.backdropLayer) !== nextIndex) return;
                layers[activeIndex].classList.remove('active', 'sliding');
                layers[activeIndex].style.transition = 'none';
                layers[activeIndex].style.transform = 'translateX(100%)';
                layers[activeIndex].style.backgroundImage = '';
                layers[nextIndex].classList.remove('sliding');
                heroSlideTimer = 0;
            }, 700);
        }).catch((error) => console.error(`Hero image failed: ${friendlyError(error)}`));
    }

    function stopHeroCarousel() {
        window.clearTimeout(heroRotationTimer);
        heroRotationTimer = 0;
    }

    function shuffledHeroCards(data, latestSections) {
        const seen = new Set();
        const cards = [];
        const sources = [
            ...(Array.isArray(data.resume) ? data.resume : []),
            ...latestSections.flatMap((section) => section.items || []),
            ...(Array.isArray(data.latest) ? data.latest : []),
        ];
        for (const card of sources) {
            if (!card?.id || seen.has(card.id) || card.type === 'CollectionFolder') continue;
            if (!card.backdropImage && !card.landscapeImage) continue;
            seen.add(card.id);
            cards.push(card);
        }
        for (let index = cards.length - 1; index > 0; index -= 1) {
            const target = Math.floor(Math.random() * (index + 1));
            [cards[index], cards[target]] = [cards[target], cards[index]];
        }
        return cards;
    }

    function startHeroCarousel(hero, cards) {
        stopHeroCarousel();
        if (!cards.length) {
            return { select: () => {}, previous: () => {}, next: () => {} };
        }
        let index = 0;
        const schedule = () => {
            stopHeroCarousel();
            if (cards.length < 2) return;
            heroRotationTimer = window.setTimeout(() => {
                if (!hero.isConnected || currentView?.kind !== 'home') return;
                if (document.hidden || !playerView.classList.contains('hidden')) {
                    schedule();
                    return;
                }
                index = (index + 1) % cards.length;
                updateHero(hero, cards[index]);
                schedule();
            }, heroRotationIntervalMs);
        };
        const select = (card) => {
            const selectedIndex = cards.findIndex((candidate) => candidate.id === card?.id);
            if (selectedIndex >= 0) index = selectedIndex;
            updateHero(hero, card);
            schedule();
        };
        const step = (offset) => {
            index = (index + offset + cards.length) % cards.length;
            updateHero(hero, cards[index]);
            schedule();
        };
        select(cards[0]);
        return {
            select,
            previous: () => step(-1),
            next: () => step(1),
        };
    }

    function renderHome(data) {
        stopHeroCarousel();
        content.replaceChildren();
        const latestSections = Array.isArray(data.latestSections)
            ? data.latestSections.filter((section) => section?.library && section.items?.length)
            : [];
        const hero = element('section', 'home-hero');
        const firstBackdrop = element('span', 'hero-backdrop-layer');
        const secondBackdrop = element('span', 'hero-backdrop-layer');
        if (lastHeroBackdropSrc) {
            firstBackdrop.style.backgroundImage = `url("${lastHeroBackdropSrc}")`;
            firstBackdrop.style.transform = 'translateX(0)';
            firstBackdrop.classList.add('active');
            hero.dataset.backdropLayer = '0';
        }
        hero.append(
            firstBackdrop,
            secondBackdrop,
            element('div', 'hero-copy'),
        );
        const heroCards = shuffledHeroCards(data, latestSections);
        const fallback = data.resume?.[0] || latestSections[0]?.items?.[0] || data.latest?.[0] || data.libraries?.[0];
        const carousel = heroCards.length
            ? startHeroCarousel(hero, heroCards)
            : { select: (card) => updateHero(hero, card), previous: () => {}, next: () => {} };
        if (!heroCards.length && fallback) carousel.select(fallback);
        if (heroCards.length > 1) {
            const controls = element('div', 'hero-carousel-controls');
            const previous = element('button', 'icon-button hero-carousel-button hero-carousel-previous');
            previous.type = 'button';
            previous.title = '上一张';
            previous.setAttribute('aria-label', '上一张海报');
            previous.append(element('span', 'hero-carousel-chevron'));
            previous.addEventListener('click', carousel.previous);
            const next = element('button', 'icon-button hero-carousel-button hero-carousel-next');
            next.type = 'button';
            next.title = '下一张';
            next.setAttribute('aria-label', '下一张海报');
            next.append(element('span', 'hero-carousel-chevron'));
            next.addEventListener('click', carousel.next);
            controls.append(previous, next);
            hero.append(controls);
        }
        const band = element('div', 'page-band');
        const focusHero = (card) => carousel.select(card);
        const libraries = createSection('媒体库', data.libraries, {
            key: 'libraries', rowIndex: 0, landscape: true, library: true, onFocus: focusHero,
            onClick: openLibrary, more: () => setCurrentView({ kind: 'libraries', data: data.libraries }),
        });
        const resume = createSection('继续观看', data.resume, {
            key: 'resume', rowIndex: 1, landscape: true, episodeAsSeries: true, onFocus: focusHero,
            onClick: (card) => card.playable ? startPlayback(card, card.resumePositionMs) : openDetail(card),
        });
        const latestRows = latestSections.length
            ? latestSections.map((section, index) => createSection(`最近添加 · ${section.library.title}`, section.items, {
                key: `latest:${section.library.id}`, rowIndex: index + 2, landscape: true,
                onFocus: focusHero, onClick: openDetail,
            }))
            : [createSection('最近添加', data.latest, {
                key: 'latest', rowIndex: 2, landscape: true, onFocus: focusHero, onClick: openDetail,
            })];
        [libraries, resume, ...latestRows].filter(Boolean).forEach((section) => band.append(section));
        if (!band.childElementCount) band.append(element('div', 'empty-state', '媒体库暂无内容'));
        content.append(hero, band);
    }

    function renderLibraries(libraries) {
        content.replaceChildren();
        const header = element('div', 'page-header');
        const back = element('button', 'back-button', '←');
        back.type = 'button'; back.title = '返回'; back.dataset.focusKey = 'back'; back.addEventListener('click', goBack);
        header.append(back, element('h1', '', '媒体库'));
        const grid = element('div', 'grid-view library-grid');
        libraries.forEach((library, index) => grid.append(createMediaCard(library, { landscape: true, library: true, index, onClick: openLibrary })));
        content.append(header, grid);
    }

    async function openLibrary(library) {
        const cached = getLibraryCache(library);
        if (cached) {
            const data = { library, ...cached, items: cached.items.slice(), syncing: true };
            setCurrentView({ kind: 'library', data });
            syncLibraryFirstPage(data);
            return;
        }
        renderLoading();
        try {
            const response = await nativeRequest('mediaStationCatalog', 'items', [
                'items',
                JSON.stringify({ parentId: library.id, startIndex: 0, limit: libraryPageSize }),
            ]);
            const page = validateLibraryPage(response, 0);
            const data = { library, ...page, syncing: false, isLoadingMore: false };
            putLibraryCache(data);
            setCurrentView({ kind: 'library', data });
        } catch (error) {
            goBack();
            showToast(friendlyError(error));
        }
    }

    function renderLibrary(data) {
        libraryPageObserver.disconnect();
        content.replaceChildren();
        const header = element('div', 'page-header');
        const back = element('button', 'back-button', '←');
        back.type = 'button'; back.title = '返回'; back.dataset.focusKey = 'back'; back.addEventListener('click', goBack);
        header.append(back, element('h1', '', data.library.title));
        const grid = element('div', 'grid-view');
        data.items.forEach((card, index) => grid.append(createMediaCard(card, { index, onClick: openDetail })));
        content.append(header, grid);
        if (data.syncError) {
            content.append(element('div', 'library-sync-error', `同步失败，当前显示缓存内容：${data.syncError}`));
        }
        content.append(element('div', 'library-page-sentinel'));
        updateLibraryPaginationUi(data);
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
        data.isLoadingMore = true;
        data.loadMoreError = '';
        updateLibraryPaginationUi(data);
        const requestedStart = data.nextStartIndex;
        try {
            const response = await nativeRequest('mediaStationCatalog', 'items', [
                'items',
                JSON.stringify({ parentId: data.library.id, startIndex: requestedStart, limit: libraryPageSize }),
            ]);
            const page = validateLibraryPage(response, requestedStart);
            const stillCurrent = currentView?.kind === 'library' && currentView.data === data;
            const grid = stillCurrent ? content.querySelector('.grid-view') : null;
            if (stillCurrent && !grid) throw new Error('媒体库网格已不可用');
            appendLibraryPage(data, page);
            data.isLoadingMore = false;
            putLibraryCache(data);
            if (!stillCurrent) return;
            const firstIndex = data.items.length - page.items.length;
            page.items.forEach((card, index) => grid.append(createMediaCard(card, { index: firstIndex + index, onClick: openDetail })));
            updateLibraryPaginationUi(data);
        } catch (error) {
            data.isLoadingMore = false;
            data.loadMoreError = friendlyError(error);
            if (currentView?.kind === 'library' && currentView.data === data) {
                updateLibraryPaginationUi(data);
                showToast(`继续加载失败：${data.loadMoreError}`);
            }
        }
    }

    async function syncLibraryFirstPage(data) {
        const scrollTop = content.scrollTop;
        let needsRender = false;
        try {
            const response = await nativeRequest('mediaStationCatalog', 'items', [
                'items',
                JSON.stringify({ parentId: data.library.id, startIndex: 0, limit: libraryPageSize }),
            ]);
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
            data.syncing = false;
            data.syncError = friendlyError(error);
            console.error(`媒体库后台同步失败：${data.syncError}`);
        }
        if (currentView?.kind === 'library' && currentView.data === data) {
            if (needsRender || data.syncError) {
                renderLibrary(data);
                restoreScroll({ scrollTop, rows: {} });
            } else {
                updateLibraryPaginationUi(data);
            }
        }
    }

    function validateLibraryPage(page, requestedStart) {
        if (!page || !Array.isArray(page.items)) throw new Error('媒体库分页响应缺少项目列表');
        const startIndex = Number(page.startIndex);
        const totalRecordCount = Number(page.totalRecordCount);
        const nextStartIndex = Number(page.nextStartIndex);
        if (!Number.isInteger(startIndex) || startIndex !== requestedStart) {
            throw new Error('媒体库分页起始位置与请求不一致');
        }
        if (!Number.isInteger(totalRecordCount) || totalRecordCount < 0) {
            throw new Error('媒体库分页总数无效');
        }
        if (!Number.isInteger(nextStartIndex) || nextStartIndex !== startIndex + page.items.length) {
            throw new Error('媒体库下一页位置无效');
        }
        if (nextStartIndex > totalRecordCount || (nextStartIndex < totalRecordCount && page.items.length === 0)) {
            throw new Error('媒体库分页提前结束或超出总数');
        }
        const ids = new Set();
        for (const item of page.items) {
            if (!item?.id || ids.has(item.id)) throw new Error('媒体库分页包含无效或重复项目');
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

    function libraryCacheKey(library) {
        return [session?.baseUrl || '', session?.userId || '', library.id, 'DateCreated', 'Descending'].join('\n');
    }

    function getLibraryCache(library) {
        const key = libraryCacheKey(library);
        const cached = libraryCache.get(key);
        if (!cached) return null;
        libraryCache.delete(key);
        libraryCache.set(key, cached);
        return cached;
    }

    function putLibraryCache(data) {
        const key = libraryCacheKey(data.library);
        libraryCache.delete(key);
        libraryCache.set(key, {
            items: data.items.slice(),
            startIndex: 0,
            nextStartIndex: data.nextStartIndex,
            totalRecordCount: data.totalRecordCount,
        });
        while (libraryCache.size > maximumLibraryCacheEntries) {
            libraryCache.delete(libraryCache.keys().next().value);
        }
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
        renderDetailLoading();
        try {
            const detail = await nativeRequest('mediaStationCatalog', 'detail', ['detail', JSON.stringify({ mediaId: card.id })]);
            setCurrentView({ kind: 'detail', data: detail });
        } catch (error) {
            goBack();
            showToast(friendlyError(error));
        }
    }

    function renderDetailLoading() {
        content.replaceChildren();
        const view = element('div', 'detail-view');
        const backdrop = element('div', 'detail-backdrop placeholder-art');
        backdrop.style.height = 'min(72vh, 720px)';
        const body = element('div', 'detail-content');
        body.append(element('button', 'icon-button detail-back placeholder-back', '←'));
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
        content.replaceChildren();
        const view = element('article', 'detail-view');
        const backdrop = element('div', 'detail-backdrop');
        const body = element('div', 'detail-content');
        const back = element('button', 'icon-button detail-back', '←');
        back.type = 'button'; back.title = '返回'; back.dataset.focusKey = 'back'; back.addEventListener('click', goBack);
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
        copy.append(element('h1', '', card.title));
        const meta = element('div', 'hero-meta');
        [card.year, formatDuration(card.durationMs), card.officialRating, card.communityRating ? `★ ${card.communityRating.toFixed(1)}` : '', card.dynamicRange]
            .filter(Boolean).forEach((value) => meta.append(element('span', value.toString().startsWith('★') ? 'rating' : '', value)));
        copy.append(meta);
        if (card.overview) {
            const overview = element('button', 'detail-overview overview-preview', card.overview);
            overview.type = 'button';
            overview.title = '查看完整简介';
            overview.dataset.focusKey = `overview:${card.id}`;
            overview.addEventListener('click', () => openOverview(card, overview));
            copy.append(overview);
        }
        const actions = element('div', 'detail-actions');
        const resumableEpisode = detail.episodes?.find((episode) => episode.resumePositionMs > 0);
        const playTarget = card.playable ? card : (resumableEpisode || detail.episodes?.[0]);
        if (playTarget?.playable) {
            const play = element('button', 'primary-command');
            play.type = 'button';
            play.dataset.focusKey = `play:${playTarget.id}`;
            const position = playTarget.resumePositionMs ? ` ${formatTime(playTarget.resumePositionMs)}` : '';
            const label = playTarget.type === 'Episode'
                ? `播放 ${episodePosition(playTarget).replace(' · ', '')}${position}`
                : (playTarget.resumePositionMs ? `继续播放${position}` : '开始播放');
            play.append(element('span', '', '▶'), element('span', '', label));
            play.addEventListener('click', () => startPlayback(playTarget, playTarget.resumePositionMs));
            actions.append(play);
            copy.append(actions);
        }
        if (card.genres?.length) {
            const tags = element('div', 'tag-list');
            card.genres.slice(0, 8).forEach((genre) => tags.append(element('span', 'tag', genre)));
            copy.append(tags);
        }
        layout.append(poster, copy);
        body.append(back, layout);
        view.append(backdrop, body);
        if (detail.episodes?.length) view.append(createEpisodes(detail.episodes));
        content.append(view);
        const ref = card.backdropImage || card.landscapeImage;
        if (ref) requestImage(ref, 1600).then((src) => { if (backdrop.isConnected) backdrop.style.backgroundImage = `url("${src}")`; }).catch((error) => console.error(`Detail image failed: ${friendlyError(error)}`));
    }

    function createEpisodes(episodes) {
        const root = element('section', 'episodes');
        const groups = new Map();
        for (const episode of episodes) {
            const season = episode.parentIndexNumber || 1;
            if (!groups.has(season)) groups.set(season, []);
            groups.get(season).push(episode);
        }
        for (const cards of groups.values()) {
            cards.sort((left, right) => (left.indexNumber || 0) - (right.indexNumber || 0));
        }
        const seasons = [...groups.keys()].sort((left, right) => left - right);
        const resumable = episodes.find((episode) => episode.resumePositionMs > 0);
        let selectedSeason = resumable?.parentIndexNumber || seasons[0];
        const heading = element('div', 'episodes-heading');
        heading.append(element('h2', '', '选集'));
        const seasonTabs = element('div', 'segmented-control season-tabs');
        seasonTabs.setAttribute('role', 'tablist');
        const stage = element('div', 'episode-stage');

        const renderSeason = (season) => {
            selectedSeason = season;
            seasonTabs.querySelectorAll('button').forEach((button) => {
                button.setAttribute('aria-selected', String(Number(button.dataset.season) === season));
            });
            const cards = groups.get(season) || [];
            const preferredIndex = Math.max(0, cards.findIndex((card) => card.resumePositionMs > 0));
            let selectedChunk = Math.floor(preferredIndex / 50);
            const chunks = [];
            for (let start = 0; start < cards.length; start += 50) chunks.push(cards.slice(start, start + 50));
            const rangeTabs = element('div', 'segmented-control episode-ranges');
            const rowHost = element('div');

            const renderChunk = (chunkIndex) => {
                selectedChunk = chunkIndex;
                rangeTabs.querySelectorAll('button').forEach((button) => {
                    button.setAttribute('aria-selected', String(Number(button.dataset.chunk) === chunkIndex));
                });
                const row = element('div', 'media-row landscape episode-row');
                row.dataset.rowKey = `season-${season}-chunk-${chunkIndex}`;
                row.dataset.rowIndex = '0';
                (chunks[chunkIndex] || []).forEach((card, index) => row.append(createMediaCard(card, {
                    landscape: true,
                    episodePicker: true,
                    omitSeriesName: true,
                    index,
                    onClick: (item) => startPlayback(item, item.resumePositionMs),
                })));
                rowHost.replaceChildren(createRowCarousel(row, '选集'));
            };

            if (chunks.length > 1) {
                chunks.forEach((chunk, index) => {
                    const first = chunk[0]?.indexNumber || index * 50 + 1;
                    const last = chunk.at(-1)?.indexNumber || first + chunk.length - 1;
                    const button = element('button', 'segment-button episode-range', `${first}-${last}`);
                    button.type = 'button';
                    button.dataset.chunk = String(index);
                    button.addEventListener('click', () => renderChunk(index));
                    rangeTabs.append(button);
                });
            }
            stage.replaceChildren();
            if (chunks.length > 1) stage.append(rangeTabs);
            stage.append(rowHost);
            renderChunk(selectedChunk);
        };

        seasons.forEach((season) => {
            const button = element('button', 'segment-button season-tab', `第 ${season} 季`);
            button.type = 'button';
            button.dataset.season = String(season);
            button.setAttribute('role', 'tab');
            button.addEventListener('click', () => renderSeason(season));
            seasonTabs.append(button);
        });
        if (seasons.length > 1) heading.append(seasonTabs);
        root.append(heading, stage);
        renderSeason(selectedSeason);
        return root;
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

    function renderSearch(data = { query: '', items: null }) {
        content.replaceChildren();
        const root = element('section', 'search-view');
        const header = element('div', 'page-header search-header');
        const back = element('button', 'back-button', '←');
        back.type = 'button';
        back.title = '返回';
        back.setAttribute('aria-label', '返回');
        back.dataset.focusKey = 'search:back';
        back.addEventListener('click', goBack);
        header.append(back, element('h1', '', '搜索'));
        const form = element('form', 'search-form');
        const field = element('div', 'search-field');
        field.append(element('span', 'search-leading', '⌕'));
        const input = document.createElement('input');
        input.type = 'search'; input.placeholder = '搜索电影、剧集'; input.value = data.query || '';
        input.setAttribute('aria-label', '搜索媒体');
        input.dataset.focusKey = 'search:input';
        const clear = element('button', 'icon-button search-clear', '×');
        clear.type = 'button';
        clear.title = '清除搜索';
        clear.setAttribute('aria-label', '清除搜索');
        clear.classList.toggle('hidden', !input.value);
        clear.addEventListener('click', () => {
            input.value = '';
            clear.classList.add('hidden');
            input.focus();
        });
        input.addEventListener('input', () => clear.classList.toggle('hidden', !input.value));
        const submit = element('button', 'primary-command', '搜索');
        submit.type = 'submit';
        field.append(input, clear);
        form.append(field, submit);
        form.addEventListener('submit', async (event) => {
            event.preventDefault();
            const query = input.value.trim();
            if (!query) return;
            submit.disabled = true;
            try {
                const result = await nativeRequest('mediaStationCatalog', 'search', ['search', JSON.stringify({ query, limit: 60 })]);
                currentView.data = { query, items: result.items };
                renderSearch(currentView.data);
            } catch (error) {
                showToast(friendlyError(error));
                submit.disabled = false;
            }
        });
        root.append(header, form);
        if (Array.isArray(data.items)) {
            if (data.items.length) {
                const resultsHeading = element('div', 'search-results-heading');
                resultsHeading.append(element('h2', '', `${data.items.length} 个结果`));
                if (data.query) resultsHeading.append(element('span', '', `“${data.query}”`));
                const grid = element('div', 'grid-view search-grid');
                data.items.forEach((card, index) => grid.append(createMediaCard(card, { index, onClick: openDetail })));
                root.append(resultsHeading, grid);
            } else {
                root.append(element('div', 'empty-state', '没有找到相关内容'));
            }
        }
        content.append(root);
        requestAnimationFrame(() => input.focus());
    }

    function openSearch() {
        setCurrentView({ kind: 'search', data: { query: '', items: null } });
    }

    function openDrawer(drawer, trigger) {
        closeDrawer(false);
        currentDrawer = drawer;
        drawerTrigger = trigger;
        byId('drawer-scrim').classList.remove('hidden');
        drawer.classList.add('open');
        drawer.setAttribute('aria-hidden', 'false');
        requestAnimationFrame(() => drawer.querySelector('button, input')?.focus());
    }

    function closeDrawer(restoreFocus = true) {
        if (!currentDrawer) return;
        currentDrawer.classList.remove('open');
        currentDrawer.setAttribute('aria-hidden', 'true');
        byId('drawer-scrim').classList.add('hidden');
        const trigger = drawerTrigger;
        currentDrawer = null;
        drawerTrigger = null;
        if (restoreFocus) trigger?.focus();
    }

    async function logout() {
        const button = byId('logout-button');
        button.disabled = true;
        try {
            await nativeRequest('mediaStationLogout', 'logout');
            closeDrawer(false);
            session = null;
            resetCatalogState();
            showLogin(byId('drawer-server').textContent || '');
        } catch (error) {
            showToast(friendlyError(error));
        } finally {
            button.disabled = false;
        }
    }

    async function loadSavedAccounts() {
        const list = byId('saved-accounts-list');
        const state = byId('saved-accounts-state');
        list.replaceChildren();
        state.textContent = '正在读取账号...';
        state.classList.remove('hidden', 'error');
        try {
            const result = await nativeRequest('mediaStationListAccounts', 'list_accounts');
            const accounts = Array.isArray(result.accounts) ? result.accounts : [];
            if (!accounts.length) {
                state.textContent = '暂无已保存账号';
                return;
            }
            if (accounts.some((account) => (
                typeof account.accountId !== 'string'
                || !/^[0-9a-f]{64}$/.test(account.accountId)
                || typeof account.baseUrl !== 'string'
                || !account.baseUrl
                || typeof account.userId !== 'string'
                || !account.userId
                || typeof account.userName !== 'string'
            ))) {
                throw new Error('已保存账号数据无效');
            }
            const isCurrent = (account) => Boolean(
                session
                && account.baseUrl === session.baseUrl
                && account.userId === session.userId
            );
            accounts.sort((left, right) => {
                const leftActive = isCurrent(left);
                const rightActive = isCurrent(right);
                if (leftActive !== rightActive) return leftActive ? -1 : 1;
                return String(left.userName || left.userId).localeCompare(
                    String(right.userName || right.userId),
                    'zh-CN',
                ) || String(left.baseUrl).localeCompare(String(right.baseUrl));
            });
            state.classList.add('hidden');
            accounts.forEach((account, index) => {
                const active = isCurrent(account);
                const button = element('button', 'saved-account-item' + (active ? ' active' : ''), '');
                button.type = 'button';
                if (active) button.setAttribute('aria-current', 'true');
                const avatar = element('span', 'avatar', (account.userName || '?').slice(0, 1).toUpperCase());
                const body = element('div', 'saved-account-copy', null);
                body.append(element('strong', '', account.userName || account.userId));
                body.append(element('span', '', account.baseUrl.replace(/^https?:\/\//, '')));
                button.append(avatar, body);
                if (active) button.append(element('span', 'saved-account-current', '当前'));
                button.style.animationDelay = `${Math.min(index * 30, 240)}ms`;
                button.addEventListener('click', () => switchAccount(account, active, button));
                list.append(button);
            });
        } catch (error) {
            const message = friendlyError(error);
            state.textContent = `账号列表加载失败：${message}`;
            state.classList.add('error');
            state.classList.remove('hidden');
            console.error(`账户列表加载失败：${message}`);
        }
    }

    async function switchAccount(account, alreadyActive, selectedButton) {
        if (alreadyActive) {
            closeDrawer();
            return;
        }
        const list = byId('saved-accounts-list');
        const state = byId('saved-accounts-state');
        list.querySelectorAll('button').forEach((button) => { button.disabled = true; });
        selectedButton?.classList.add('switching');
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
            closeDrawer(false);
            resetCatalogState();
            await showApp(status);
        } catch (error) {
            const message = friendlyError(error);
            state.textContent = `账号切换失败：${message}`;
            state.classList.add('error');
            showToast(message);
            list.querySelectorAll('button').forEach((button) => { button.disabled = false; });
            selectedButton?.classList.remove('switching');
            spinner.remove();
        } finally {
            selectedButton?.removeAttribute('aria-busy');
        }
    }

    async function startPlayback(card, startMs = 0) {
        if (!card?.id || !card.playable) {
            showToast('该项目不能直接播放');
            return;
        }
        const activePlayer = {
            card,
            playing: true,
            started: false,
            buffering: false,
            exitArmed: false,
            positionMs: startMs || 0,
            durationMs: card.durationMs || 0,
            loadInfo: null,
            panelKind: '',
            trackChanging: false,
            trackRefreshing: false,
            scrubbing: false,
            scrubPositionMs: null,
            interpolationEnabled: false,
            interpolationModel: null,
            interpolationChanging: false,
            interpolationTargetModel: null,
            interpolationResumePlaying: true,
        };
        player = activePlayer;
        byId('player-title').textContent = cardTitle(card, true);
        byId('player-subtitle').textContent = card.type === 'Episode' ? episodePosition(card) : cardSubtitle(card);
        byId('player-loading-title').textContent = cardTitle(card, true);
        setPlayerPoster(card);
        setPlayerLoading(true, '正在准备播放');
        updatePlayerProgress();
        playerPanel.classList.add('hidden');
        playerPanelTrigger = null;
        refreshPlayerTools();
        hidePlayerFeedback();
        playerView.classList.remove('hidden');
        setPlayerMode(true);
        showPlayerControls();
        try {
            await waitForPlayerPaint();
            const loadInfo = await nativeRequest(
                'mediaStationLoad',
                'load',
                [card.id, Math.max(0, Math.round(startMs || 0)), 'off', preferredInterpolationModel],
                60000,
            );
            if (player === activePlayer) {
                activePlayer.loadInfo = loadInfo;
                refreshPlayerTools();
            }
        } catch (error) {
            if (player === activePlayer) {
                showToast(friendlyError(error));
                finishPlayer(false);
            }
        }
    }

    function handlePlaybackEvent(event) {
        // The stopped-session report has landed server-side; refresh the home
        // catalog so Continue Watching progress is current. Must run before
        // the !player guard below because it arrives after the player closed.
        if (event.kind === 'home_stale') {
            scheduleHomeRefresh(200);
            return;
        }
        if (!player) return;
        if (event.kind === 'canceled' && player.interpolationChanging) return;
        if (Number.isFinite(event.positionMs) && !player.scrubbing) player.positionMs = event.positionMs;
        if (Number.isFinite(event.durationMs) && event.durationMs > 0) player.durationMs = event.durationMs;
        updatePlayerProgress();
        switch (event.kind) {
            case 'started':
                {
                    const firstFrame = !player.started;
                    const interpolationChanged = player.interpolationChanging;
                    const shouldPlay = interpolationChanged ? player.interpolationResumePlaying : true;
                    if (interpolationChanged) {
                        player.interpolationEnabled = player.interpolationTargetModel !== null;
                        player.interpolationModel = player.interpolationTargetModel;
                        player.interpolationChanging = false;
                        player.interpolationTargetModel = null;
                    }
                    player.started = true;
                    player.playing = shouldPlay;
                    player.buffering = false;
                    if (!shouldPlay && window.jmpNative) window.jmpNative.playerPause();
                    refreshPlayerTools();
                    setPlayerLoading(false);
                    if (firstFrame && !player.panelKind) {
                        hidePlayerFeedback();
                        hidePlayerControls();
                    } else {
                        hidePlayerFeedback();
                        showPlayerControls();
                    }
                }
                break;
            case 'paused':
                if (player.interpolationChanging) {
                    player.playing = player.interpolationResumePlaying;
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
                hidePlayerFeedback();
                showPlayerControls();
                break;
            case 'buffering':
                player.buffering = event.buffering === true;
                if (player.interpolationChanging) {
                    setPlayerLoading(true, interpolationLoadingLabel(player));
                } else if (player.buffering) {
                    setPlayerLoading(
                        true,
                        player.started ? '正在缓冲' : '正在准备播放',
                    );
                } else {
                    setPlayerLoading(!player.started, '正在准备播放');
                }
                break;
            case 'finished': case 'canceled':
                finishPlayer(false);
                break;
            case 'error':
                showToast(event.errorCode
                    ? friendlyError({ code: event.errorCode })
                    : (player.interpolationChanging ? 'RTX 插帧切换失败' : '播放失败'));
                finishPlayer(false);
                break;
        }
    }

    function setPlayerLoading(visible, label = '') {
        if (label) byId('player-loading-label').textContent = label;
        byId('player-loading').classList.toggle('hidden', !visible);
        const covered = visible && (!player?.started || player?.interpolationChanging);
        playerView.classList.toggle('preparing', covered);
    }

    function setPlayerPoster(card) {
        const img = byId('player-poster').firstElementChild;
        const ref = card.backdropImage || card.landscapeImage || card.primaryImage;
        if (!ref) return;
        img.removeAttribute('src');
        img.classList.remove('image-ready', 'image-error');
        requestImage(ref, 640)
            .then((src) => { if (img.isConnected) setImageSource(img, src); })
            .catch(() => {});
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
        if (target === null || !player) return;
        player.scrubbing = true;
        player.scrubPositionMs = target;
        updatePlayerProgress();
    }

    function commitProgressSeek(value) {
        const target = progressTargetMs(value);
        if (target === null || !player || !window.jmpNative) {
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
        if (!player || !window.jmpNative) return;
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
    }

    async function setFrameInterpolation(modelId) {
        const activePlayer = player;
        if (!activePlayer?.started || !activePlayer.loadInfo || activePlayer.interpolationChanging) return;
        if (modelId !== null && !Object.prototype.hasOwnProperty.call(interpolationModels, modelId)) {
            const error = new Error('所选 RIFE 模型无效');
            error.code = 'frame_interpolation_model_invalid';
            showToast(friendlyError(error));
            return;
        }
        const requestedModel = modelId;
        const activeModel = activePlayer.interpolationEnabled
            ? (activePlayer.interpolationModel || activePlayer.loadInfo.frameInterpolation?.modelId)
            : null;
        if (requestedModel === activeModel) {
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
        setPlayerLoading(true, interpolationLoadingLabel(activePlayer));
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
                if (player !== activePlayer) return;
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
                ],
                60000,
            );
            if (player !== activePlayer) return;
            const actualModel = loadInfo.frameInterpolation?.modelId || null;
            if (actualModel !== requestedModel) {
                const error = new Error('播放器返回的 RTX 插帧状态与请求不一致');
                error.code = 'frame_interpolation_state_mismatch';
                throw error;
            }
            activePlayer.loadInfo = loadInfo;
            if (activePlayer.interpolationChanging) {
                setPlayerLoading(true, interpolationLoadingLabel(activePlayer));
                if (window.jmpNative) window.jmpNative.playerPlay();
            }
            refreshPlayerTools();
        } catch (error) {
            if (player !== activePlayer) return;
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
        if (!player || !window.jmpNative) return;
        window.jmpNative.toggleFullscreen();
        showPlayerControls();
    }

    function isPlayerInteractiveTarget(target) {
        return target instanceof Element && Boolean(target.closest('button, input, .player-panel'));
    }

    function seekBy(deltaMs) {
        if (!player || !window.jmpNative || !player.durationMs) return;
        const target = Math.max(0, Math.min(player.durationMs, player.positionMs + deltaMs));
        player.positionMs = target;
        updatePlayerProgress();
        window.jmpNative.playerSeek(Math.round(target));
    }

    function refreshPlayerTools() {
        const info = player?.loadInfo;
        const playback = byId('player-playback');
        const subtitles = byId('player-subtitles');
        const audio = byId('player-audio');
        const interpolation = byId('player-interpolation');
        const infoButton = byId('player-info');
        const fullscreen = byId('player-fullscreen');
        const playing = player?.playing === true;
        const fullscreenActive = window._isFullscreen === true;
        playback.disabled = !player;
        playback.classList.toggle('is-playing', playing);
        playback.title = playing ? '暂停' : '播放';
        playback.setAttribute('aria-label', playback.title);
        playback.querySelector('span').textContent = playing ? pauseSymbol : playSymbol;
        subtitles.disabled = !info || !Array.isArray(info.subtitleTracks);
        audio.disabled = !info || !Array.isArray(info.audioTracks) || !info.audioTracks.length;
        const interpolationEnabled = player?.interpolationChanging
            ? player.interpolationTargetModel !== null
            : player?.interpolationEnabled === true;
        const interpolationModel = player?.interpolationChanging
            ? player.interpolationTargetModel
            : (player?.interpolationModel || info?.frameInterpolation?.modelId || null);
        interpolation.disabled = !player?.started || !info || player.interpolationChanging;
        interpolation.title = player?.interpolationChanging
            ? interpolationLoadingLabel(player)
            : (interpolationEnabled
                ? `RTX 插帧：${interpolationModels[interpolationModel]?.label || '已开启'}`
                : 'RTX 插帧');
        interpolation.setAttribute('aria-label', interpolation.title);
        interpolation.setAttribute('aria-pressed', String(interpolationEnabled));
        infoButton.classList.toggle('hidden', !playbackInfoEnabled);
        infoButton.disabled = !playbackInfoEnabled || !info;
        fullscreen.disabled = !player;
        fullscreen.classList.toggle('is-fullscreen', fullscreenActive);
        fullscreen.title = fullscreenActive ? '退出全屏' : '进入全屏';
        fullscreen.setAttribute('aria-label', fullscreen.title);
        fullscreen.setAttribute('aria-pressed', String(fullscreenActive));
        const activeKind = player?.panelKind || '';
        subtitles.setAttribute('aria-pressed', String(activeKind === 'subtitles'));
        audio.setAttribute('aria-pressed', String(activeKind === 'audio'));
        infoButton.setAttribute('aria-pressed', String(activeKind === 'info'));
    }

    function trackTitle(track, fallback) {
        return String(track?.label || track?.language || fallback || '').trim() || fallback;
    }

    function trackMeta(track, kind) {
        const parts = [];
        if (track?.language && track.label !== track.language) parts.push(track.language);
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

    function renderPlayerPanel() {
        const info = player?.loadInfo;
        const kind = player?.panelKind;
        playerPanelContent.replaceChildren();
        if (!info || !kind) return;

        if (kind === 'subtitles') {
            byId('player-panel-title').textContent = '字幕';
            const group = element('div');
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
                    title: trackTitle(track, `字幕 ${index + 1}`),
                    meta: trackMeta(track, 'subtitle'),
                    selected: enabled && info.subtitleTrackKey === track.key,
                    onSelect: () => selectPlayerTrack('subtitle', track.key),
                }));
            }
            playerPanelContent.append(group);
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

        if (kind === 'interpolation') {
            byId('player-panel-title').textContent = 'RTX 插帧';
            const selectedModel = player.interpolationChanging
                ? player.interpolationTargetModel
                : (player.interpolationEnabled
                    ? (player.interpolationModel || info.frameInterpolation?.modelId)
                    : null);
            const group = element('div');
            group.append(createTrackOption({
                title: '关闭',
                meta: '',
                selected: selectedModel === null,
                onSelect: () => setFrameInterpolation(null),
            }));
            for (const [modelId, model] of Object.entries(interpolationModels)) {
                group.append(createTrackOption({
                    title: model.label,
                    meta: model.name,
                    selected: selectedModel === modelId,
                    onSelect: () => setFrameInterpolation(modelId),
                }));
            }
            playerPanelContent.append(group);
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
        const interpolation = createInfoSection('RTX 插帧');
        const activeInterpolation = info.frameInterpolation;
        const diagnostics = info.frameInterpolationDiagnostics || {};
        if (activeInterpolation) {
            appendInfoRow(interpolation.list, '模式 / 输出', `${activeInterpolation.mode} · ${activeInterpolation.targetFps} fps`);
            appendInfoRow(interpolation.list, '后端 / 运行库', `${activeInterpolation.backend} · ${activeInterpolation.runtimeVersion}`);
            appendInfoRow(interpolation.list, '模型 / Engine', `${activeInterpolation.model} · ${activeInterpolation.engineKey}`);
            appendInfoRow(interpolation.list, '精度 / Scale', `${activeInterpolation.precision} · ${activeInterpolation.scale}`);
            appendInfoRow(interpolation.list, '硬件解码', infoValue(diagnostics.hwdecCurrent || activeInterpolation.hwdec));
        } else {
            appendInfoRow(interpolation.list, '状态', '关闭');
        }
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

    async function openPlayerPanel(kind, trigger) {
        const activePlayer = player;
        if (!activePlayer?.loadInfo || activePlayer.trackRefreshing) return;
        if (activePlayer.panelKind === kind && !playerPanel.classList.contains('hidden')) {
            closePlayerPanel(true);
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
                if (player !== activePlayer) return;
                activePlayer.loadInfo.audioTracks = tracks.audioTracks;
                activePlayer.loadInfo.subtitleTracks = tracks.subtitleTracks;
                activePlayer.loadInfo.audioTrackKey = tracks.audioTrackKey || null;
                activePlayer.loadInfo.subtitleTrackKey = tracks.subtitleTrackKey || null;
                activePlayer.loadInfo.subtitleEnabled = tracks.subtitleEnabled === true;
            } catch (error) {
                if (player === activePlayer) showToast(friendlyError(error));
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
                if (player !== activePlayer) return;
                activePlayer.loadInfo.frameInterpolation = diagnostics.active || null;
                activePlayer.loadInfo.frameInterpolationDiagnostics = diagnostics.playback || {};
                updateFrameInterpolationStatus(diagnostics.status);
            } catch (error) {
                console.error(`RTX frame interpolation diagnostics failed: ${friendlyError(error)}`);
                showToast(friendlyError(error));
            }
        }
        if (player !== activePlayer) return;
        activePlayer.panelKind = kind;
        playerPanelTrigger = trigger || null;
        window.clearTimeout(controlsTimer);
        playerControls.classList.add('visible');
        activePlayer.exitArmed = false;
        renderPlayerPanel();
        playerPanel.classList.remove('hidden');
        refreshPlayerTools();
        window.setTimeout(() => {
            const selected = playerPanelContent.querySelector('[aria-checked="true"]');
            if (selected) selected.focus();
            else if (playerPanelTrigger?.isConnected) playerPanelTrigger.focus();
        }, 0);
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

    async function selectPlayerTrack(kind, key) {
        const activePlayer = player;
        if (!activePlayer || activePlayer.trackChanging) return;
        activePlayer.trackChanging = true;
        playerPanelContent.querySelectorAll('button').forEach((button) => { button.disabled = true; });
        try {
            const result = await nativeRequest(
                'mediaStationSelectTrack',
                'track_selection',
                [activePlayer.card.id, kind, key],
                45000,
            );
            if (player !== activePlayer) return;
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
            if (player === activePlayer) showToast(friendlyError(error));
            renderPlayerPanel();
        } finally {
            if (player === activePlayer) activePlayer.trackChanging = false;
        }
    }

    function movePlayerPanelFocus(delta) {
        const options = [...playerPanelContent.querySelectorAll('.track-option')];
        if (!options.length) {
            playerPanelContent.scrollBy({ top: delta * 88, behavior: 'smooth' });
            return;
        }
        const current = options.indexOf(document.activeElement);
        const next = options[Math.max(0, Math.min(options.length - 1, current < 0 ? 0 : current + delta))];
        if (next) focusAndReveal(next, 'nearest', 'nearest');
    }

    function finishPlayer(stopNative = true) {
        if (stopNative && player && window.jmpNative) window.jmpNative.playerStop();
        // Optimistically update Continue Watching with the position we last
        // saw and move the just-ended item to the front, so it reflects the
        // playback immediately; the network refresh (and the home_stale
        // fallback) then reconciles with the authoritative server value.
        if (player && player.positionMs > 0 && homeData && Array.isArray(homeData.resume)) {
            const id = player.card?.id;
            const index = id ? homeData.resume.findIndex((item) => item.id === id) : -1;
            if (index >= 0) {
                const item = homeData.resume[index];
                if (player.positionMs > (item.resumePositionMs || 0)) {
                    item.resumePositionMs = player.positionMs;
                }
                if (index !== 0) {
                    homeData.resume.splice(index, 1);
                    homeData.resume.unshift(item);
                }
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
        refreshPlayerCursor();
        setPlayerMode(false);
        appShell.classList.remove('hidden');
        // Re-render the home view immediately with the optimistically updated
        // Continue Watching progress, then refresh the catalog right away so
        // the server value lands; the "home_stale" event refreshes again as
        // the authoritative fallback.
        if (currentView?.kind === 'home' && homeData) {
            const saved = captureView();
            currentView = { ...currentView, data: homeData };
            renderHome(homeData);
            if (saved) restoreScroll(saved);
        }
        scheduleHomeRefresh(300);
        if (playerControls.contains(document.activeElement)) document.activeElement.blur();
    }

    function showPlayerControls(focusTarget = '') {
        if (player?.playing && feedbackTimer) hidePlayerFeedback();
        playerControls.classList.add('visible');
        if (player) player.exitArmed = focusTarget === 'exit';
        if (focusTarget === 'exit') focusElement(byId('player-exit'));
        refreshPlayerCursor();
        scheduleControlsHide();
    }

    function hidePlayerControls() {
        if (player?.panelKind) return;
        playerControls.classList.remove('visible');
        if (player) player.exitArmed = false;
        if (playerControls.contains(document.activeElement)) document.activeElement.blur();
        refreshPlayerCursor();
    }

    function refreshPlayerCursor() {
        const shouldHide = Boolean(player && window._isFullscreen === true && !playerControls.classList.contains('visible'));
        playerView.classList.toggle('cursor-hidden', shouldHide);
    }

    function showPlayerFeedback(symbol, durationMs = 0, onElapsed = null) {
        window.clearTimeout(feedbackTimer);
        const feedback = byId('player-feedback');
        feedback.querySelector('span').textContent = symbol;
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
        if (!player?.playing || !player.started || player.panelKind) return;
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

    byId('brand-home').addEventListener('click', goHome);
    byId('topbar-home-shortcut').addEventListener('click', goHome);
    byId('nav-home').addEventListener('click', goHome);
    byId('nav-library').addEventListener('click', () => homeData && setCurrentView({ kind: 'libraries', data: homeData.libraries }));
    byId('search-open').addEventListener('click', openSearch);
    byId('account-open').addEventListener('click', (event) => {
        openDrawer(byId('account-drawer'), event.currentTarget);
        loadSavedAccounts();
    });
    byId('settings-open').addEventListener('click', (event) => {
        openDrawer(byId('settings-drawer'), event.currentTarget);
        refreshFrameInterpolationStatus();
    });
    byId('drawer-scrim').addEventListener('click', () => closeDrawer());
    document.querySelectorAll('.drawer-close').forEach((button) => button.addEventListener('click', () => closeDrawer()));
    byId('overview-close').addEventListener('click', closeOverview);
    byId('overview-scrim').addEventListener('click', (event) => {
        if (event.target === event.currentTarget) closeOverview();
    });
    byId('logout-button').addEventListener('click', logout);
    byId('change-account-button').addEventListener('click', beginAccountChange);
    byId('login-cancel').addEventListener('click', cancelAccountChange);
    const playbackInfoToggle = byId('playback-info-enabled');
    playbackInfoToggle.checked = playbackInfoEnabled;
    playbackInfoToggle.addEventListener('change', () => {
        const requested = playbackInfoToggle.checked;
        try {
            window.localStorage.setItem(playbackInfoSettingKey, String(requested));
            playbackInfoEnabled = requested;
            refreshPlayerTools();
        } catch (error) {
            playbackInfoToggle.checked = playbackInfoEnabled;
            console.error(`MediaStation playback info setting could not be saved: ${error}`);
            showToast('播放信息设置保存失败');
        }
    });
    byId('clear-image-cache').addEventListener('click', async (event) => {
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
            restoreScroll(currentView);
            updateImageCacheStatus();
            showToast('图片缓存已清除');
        } catch (error) {
            console.error(`图片缓存清理失败：${friendlyError(error)}`);
            showToast(`图片缓存清理失败：${friendlyError(error)}`);
        } finally {
            button.disabled = false;
        }
    });
    byId('player-exit').addEventListener('click', () => finishPlayer(true));
    byId('player-playback').addEventListener('click', togglePlayback);
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
        if (!player || isPlayerInteractiveTarget(event.target)) return;
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
        const inputActive = ['INPUT', 'TEXTAREA'].includes(document.activeElement?.tagName);
        if (player) {
            const active = document.activeElement;
            const toolButtons = [byId('player-playback'), byId('player-subtitles'), byId('player-audio'), byId('player-interpolation'), byId('player-info'), byId('player-fullscreen')]
                .filter((button) => !button.disabled);
            if (!playerPanel.classList.contains('hidden')) {
                if (event.key === 'Escape' || event.key === 'Backspace') {
                    event.preventDefault(); closePlayerPanel(true);
                } else if ((event.key === 'Enter' || event.key === ' ') && toolButtons.includes(active)) {
                    event.preventDefault(); active.click();
                } else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
                    event.preventDefault(); movePlayerPanelFocus(event.key === 'ArrowDown' ? 1 : -1);
                }
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
                if (active === byId('player-exit') || toolButtons.includes(active)) active.click();
                else if (playerControls.classList.contains('visible') && player.exitArmed) finishPlayer(true);
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
            if (loginCanCancel) cancelAccountChange();
            else goBack();
            return;
        }
        if (inputActive || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
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
        const card = document.activeElement?.closest?.('.media-card');
        if (!card) return;
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
                if (event.key === 'ArrowUp' && row.classList.contains('episode-row')) {
                    const target = content.querySelector('.episode-ranges [aria-selected="true"]')
                        || content.querySelector('.season-tabs [aria-selected="true"]');
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
        if (!(row instanceof Element) || !row.classList.contains('media-row')) return;
        row.classList.add('is-scrolling');
        window.clearTimeout(rowScrollIdleTimers.get(row));
        rowScrollIdleTimers.set(row, window.setTimeout(() => {
            row.classList.remove('is-scrolling');
            rowScrollIdleTimers.delete(row);
        }, 120));
    }, { capture: true, passive: true });

    window.addEventListener('resize', () => {
        content.querySelectorAll('.carousel-row').forEach((row) => row._refreshCarousel?.());
    }, { passive: true });

    content.addEventListener('pointerdown', (event) => {
        stopSmoothScroll(content);
        const row = event.target.closest?.('.media-row');
        if (row) stopSmoothScroll(row);
    }, { passive: true });

    document.addEventListener('keyup', (event) => {
        if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') seekRepeatCount = 0;
    });

    initialize();
})();
