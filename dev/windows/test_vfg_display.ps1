param([ValidateRange(1, 600)][int]$Seconds = 60, [string]$ResultDirectory = '',
    [switch]$ReportPartialSampling)
$ErrorActionPreference = 'Stop'
$Repo = (Get-Item $PSScriptRoot).Parent.Parent.FullName
if (-not $ResultDirectory) { $ResultDirectory = Join-Path $Repo ('build/vfg-display-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
if (Test-Path -LiteralPath $ResultDirectory) { throw 'Result directory already exists' }
New-Item -ItemType Directory -Path $ResultDirectory | Out-Null
$ResultDirectory = (Resolve-Path -LiteralPath $ResultDirectory).Path
$Context = Join-Path $Repo 'third_party/mpv/video/out/d3d11/context.c'
$Renderer = Join-Path $Repo 'third_party/mpv/video/out/vo_gpu_next.c'
$Dll = Join-Path $Repo 'third_party/mpv-install/lib/libmpv-2.dll'
if (Get-Process -Name vfgbench -ErrorAction SilentlyContinue) { throw 'Benchmark still running' }
$OriginalContext = [IO.File]::ReadAllBytes($Context)
$OriginalRenderer = [IO.File]::ReadAllBytes($Renderer)
$OriginalDll = [IO.File]::ReadAllBytes($Dll)
function Replace-Once([string]$Text, [string]$From, [string]$To) {
    if (($Text.Split($From)).Count -ne 2) { throw "Unexpected source anchor: $From" }
    return $Text.Replace($From, $To)
}
try {
    $Source = [Text.Encoding]::UTF8.GetString($OriginalContext).Replace("`r`n", "`n")
    $Worker = @'
// Measurement only: preserve the actual refresh timestamp, never poll time.
static DWORD WINAPI vfg_trace_display(void *arg)
{
    struct ra_ctx *ctx = arg;
    struct priv *p = ctx->priv;
    UINT previous = 0;
    HRESULT previous_hr = E_PENDING;
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    HANDLE wait_handles[] = {p->trace_stop, p->trace_timer};
    while (WaitForMultipleObjects(2, wait_handles, FALSE, INFINITE) == WAIT_OBJECT_0 + 1) {
        DXGI_FRAME_STATISTICS measured = {0};
        HRESULT hr = IDXGISwapChain_GetFrameStatistics(p->swapchain, &measured);
        if (measured.PresentCount == previous && hr == previous_hr)
            continue;
        previous = measured.PresentCount;
        previous_hr = hr;
        LARGE_INTEGER now;
        QueryPerformanceCounter(&now);
        MP_INFO(ctx, "VFG_DXGI_STATS hr=%08lx present=%u refresh=%u sync=%u qpc=%lld query=%lld freq=%lld\n",
            (unsigned long)hr, measured.PresentCount, measured.PresentRefreshCount,
            measured.SyncRefreshCount, (long long)measured.SyncQPCTime.QuadPart,
            (long long)now.QuadPart, (long long)p->perf_freq);
    }
    return 0;
}

static struct ra_tex *get_backbuffer(struct ra_ctx *ctx)
'@
    $Source = Replace-Once $Source 'static struct ra_tex *get_backbuffer(struct ra_ctx *ctx)' $Worker
    $Source = Replace-Once $Source '    int64_t perf_freq;' "    HANDLE trace_stop;`n    HANDLE trace_timer;`n    HANDLE trace_thread;`n    int64_t perf_freq;"
    $Source = Replace-Once $Source "    return true;`n`nerror:`n    d3d11_uninit(ctx);" @'
    p->trace_stop = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (!p->trace_stop) goto error;
    p->trace_timer = CreateWaitableTimerExW(NULL, NULL,
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
    if (!p->trace_timer) goto error;
    LARGE_INTEGER trace_due = {.QuadPart = -10000};
    if (!SetWaitableTimer(p->trace_timer, &trace_due, 1, NULL, NULL, FALSE)) goto error;
    p->trace_thread = CreateThread(NULL, 0, vfg_trace_display, ctx, 0, NULL);
    if (!p->trace_thread) goto error;
    return true;

error:
    d3d11_uninit(ctx);
'@
    $Source = Replace-Once $Source '    SAFE_RELEASE(p->swapchain);' @'
    if (p->trace_thread) {
        SetEvent(p->trace_stop);
        WaitForSingleObject(p->trace_thread, INFINITE);
        CloseHandle(p->trace_thread);
    }
    if (p->trace_stop) CloseHandle(p->trace_stop);
    if (p->trace_timer) CloseHandle(p->trace_timer);
    SAFE_RELEASE(p->swapchain);
'@
    $Source = Replace-Once $Source '    IDXGISwapChain_Present(p->swapchain, p->opts->sync_interval, 0);' @'
    IDXGISwapChain_Present(p->swapchain, p->opts->sync_interval, 0);
    // The render thread also samples after Present. This covers worker
    // scheduling gaps without inferring a timestamp for any missing event.
    DXGI_FRAME_STATISTICS measured = {0};
    HRESULT hr = IDXGISwapChain_GetFrameStatistics(p->swapchain, &measured);
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    MP_INFO(sw->ctx, "VFG_DXGI_STATS hr=%08lx present=%u refresh=%u sync=%u qpc=%lld query=%lld freq=%lld\n",
        (unsigned long)hr, measured.PresentCount, measured.PresentRefreshCount,
        measured.SyncRefreshCount, (long long)measured.SyncQPCTime.QuadPart,
        (long long)now.QuadPart, (long long)p->perf_freq);
'@
    [IO.File]::WriteAllText($Context, $Source, [Text.UTF8Encoding]::new($false))
    $Source = [Text.Encoding]::UTF8.GetString($OriginalRenderer).Replace("`r`n", "`n")
    $Source = Replace-Once $Source '#include "video/out/d3d11/ra_d3d11.h"' "#include `"video/out/d3d11/ra_d3d11.h`"`n#include `"video/out/d3d11/context.h`""
    $Source = Replace-Once $Source '    double last_pts;' "    double last_pts;`n    double trace_pts;`n    int trace_mix_frames;"
    $Source = Replace-Once $Source '    p->is_interpolated = pts_offset != 0 && mix.num_frames > 1;' @'
    // Identify the source image actually selected by libplacebo, rather than
    // assuming every swap-chain Present contains new video content.
    p->trace_mix_frames = mix.num_frames;
    p->trace_pts = mix.num_frames == 1
        ? ((struct mp_image *)mix.frames[0]->user_data)->pts : MP_NOPTS_VALUE;
    p->is_interpolated = pts_offset != 0 && mix.num_frames > 1;
'@
    $Source = Replace-Once $Source '    sw->fns->swap_buffers(sw);' @'
    sw->fns->swap_buffers(sw);
#if HAVE_D3D11 && defined(PL_HAVE_D3D11)
    IDXGISwapChain *chain = ra_d3d11_ctx_get_swapchain(p->ra_ctx);
    if (chain) {
        UINT present = 0;
        HRESULT hr = IDXGISwapChain_GetLastPresentCount(chain, &present);
        MP_INFO(vo, "VFG_CONTENT_PRESENT hr=%08lx present=%u pts=%.9f mix=%d\n",
            (unsigned long)hr, present, p->trace_pts, p->trace_mix_frames);
        IDXGISwapChain_Release(chain);
    }
#endif
'@
    [IO.File]::WriteAllText($Renderer, $Source, [Text.UTF8Encoding]::new($false))
    Copy-Item -LiteralPath $Context -Destination (Join-Path $ResultDirectory 'instrumented-context.c')
    Copy-Item -LiteralPath $Renderer -Destination (Join-Path $ResultDirectory 'instrumented-renderer.c')
    & (Join-Path $PSScriptRoot 'build_mpv_source.ps1') -Force *> (Join-Path $ResultDirectory 'build.log')
    if ($LASTEXITCODE -ne 0) { throw 'Measurement build failed; inspect build.log' }
    Copy-Item -LiteralPath $Dll -Destination (Join-Path $ResultDirectory 'measurement-libmpv-2.dll')
    foreach ($Size in @('1920x1080', '3840x2160')) {
        foreach ($Sync in @('audio', 'display-vdrop')) {
            $Case = Join-Path $ResultDirectory "$Size-$Sync"
            & (Join-Path $PSScriptRoot 'test_vfg_cadence.ps1') -Seconds $Seconds -SourceSize $Size -VideoSync $Sync -ResultDirectory $Case *> "$Case.log"
            if ($LASTEXITCODE -ne 0) { throw "Playback failed: $Case.log" }
            & (Join-Path $PSScriptRoot 'analyze_vfg_display.ps1') -ResultDirectory $Case -ReportOnly:$ReportPartialSampling
        }
    }
} finally {
    [IO.File]::WriteAllBytes($Context, $OriginalContext)
    [IO.File]::WriteAllBytes($Renderer, $OriginalRenderer)
    [IO.File]::WriteAllBytes($Dll, $OriginalDll)
    Write-Output 'Restored production mpv source and candidate DLL; diagnostic DLL was used only for measurement.'
}
