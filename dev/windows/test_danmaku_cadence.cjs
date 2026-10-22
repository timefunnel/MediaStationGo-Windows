// Hardware acceptance gate for analyze.py output from a native CEF/desktop run.
// Pass a two-case summary JSON; callback FPS alone must never satisfy this test.
const assert = require('node:assert/strict');
const fs = require('node:fs');

assert.equal(process.argv.length, 3, 'Usage: node dev/windows/test_danmaku_cadence.cjs <summary.json>');
const cases = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
assert.equal(cases.length, 2, 'Both window and fullscreen cases are required');
const failures = [];
for (const sample of cases) {
    function check(condition, message) {
        if (!condition) failures.push(`${sample.case}: ${message}`);
    }
    check(sample.measurementSeconds >= 59, 'at least 59 seconds of steady-state data required');
    check(sample.knownIntervalTimeCoveragePercent >= 99, 'desktop interval coverage below 99%');
    check(sample.markerValidPercent === 100, 'invalid desktop frame identifiers');
    check(sample.foregroundCoveragePercent === 100, 'application lost foreground');
    check(sample.nativeErrors.length === 0, 'native playback reported errors');
    check(sample.desktopObservedDifferentFps >= sample.refreshHz * 0.99,
        `desktop throughput ${sample.desktopObservedDifferentFps.toFixed(2)} FPS below 99% of ${sample.refreshHz} Hz`);
    check(sample.desktopConfirmedIntervalMs.p99 <= sample.idealFrameMs * 1.5,
        `desktop P99 ${sample.desktopConfirmedIntervalMs.p99.toFixed(2)} ms exceeds 1.5 refresh periods`);
    console.log(`${sample.case}: desktop=${sample.desktopObservedDifferentFps.toFixed(2)} FPS, P99=${sample.desktopConfirmedIntervalMs.p99.toFixed(2)} ms, coverage=${sample.knownIntervalTimeCoveragePercent.toFixed(3)}%`);
}
if (failures.length) {
    console.error(failures.join('\n'));
    process.exitCode = 1;
} else {
    console.log('Native danmaku cadence acceptance passed (desktop capture conditions).');
}
