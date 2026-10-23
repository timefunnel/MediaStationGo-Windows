#include "vfg_timing.h"
#include <assert.h>
#include <stdio.h>

static void plan(double source, double display, int divisor, double target)
{
    struct vfg_plan p;
    assert(vfg_plan_divisor(24000.0 / 1001, 152, 2, &p));
    assert(p.target_fps == 76 && p.display_divisor == 2);
    assert(vfg_plan_divisor(24, 152, 3, &p));
    assert(fabs(p.target_fps - 152.0 / 3) < 1e-8);
    assert(!vfg_plan_divisor(24, 152, 0, &p));
    assert(!vfg_plan_divisor(24, 152, -1, &p));
    assert(!vfg_plan_divisor(24, 152, 7, &p));
    assert(!vfg_plan_divisor(NAN, 152, 2, &p));
    assert(!vfg_plan_divisor(24, INFINITY, 2, &p));
    assert(vfg_plan_auto(source, display, &p));
    assert(p.display_divisor == divisor);
    assert(fabs(p.target_fps - target) < 1e-8);
    assert(p.target_fps > source && p.target_fps <= display);
    assert(p.target_fps <= 3 * source);
    assert(fabs(p.target_fps * divisor - display) < 1e-8);
}

int main(void)
{
    plan(24, 152, 3, 152.0 / 3);
    plan(24000.0 / 1001, 152, 3, 152.0 / 3);
    plan(24, 144, 3, 48);
    plan(24, 120, 3, 40);
    plan(24, 60, 1, 60);
    plan(30000.0 / 1001, 119.88, 2, 59.94);
    plan(30, 152, 3, 152.0 / 3);
    plan(60, 144, 1, 144);
    struct vfg_plan p;
    assert(!vfg_plan_auto(24, 0, &p));
    assert(!vfg_plan_auto(0, 152, &p));
    assert(!vfg_plan_auto(NAN, 152, &p));
    assert(!vfg_plan_auto(24, INFINITY, &p));
    assert(!vfg_plan_auto(60, 60, &p));
    assert(!vfg_plan_auto(120, 60, &p));
    /* Long-film clock: calculate every timestamp from the same origin rather
     * than accumulating rounded intervals; source time never gets rescaled. */
    struct vfg_clock clock;
    double origin = 7.125, fps = 152.0 / 3, previous = origin;
    vfg_clock_start(&clock, origin, fps);
    for (uint64_t i = 1; i <= 1824000; i++) {
        double pts = vfg_clock_next(&clock);
        assert(pts > previous);
        assert(fabs(pts - previous - 1 / fps) < 1e-10);
        assert(fabs(pts - origin - (double)i / fps) < 1e-10);
        previous = pts;
        vfg_clock_advance(&clock);
    }
    /* Exact seek resets its phase instead of mixing the old frame history. */
    vfg_clock_start(&clock, 123.456, 60);
    assert(fabs(vfg_clock_next(&clock) - (123.456 + 1.0 / 60)) < 1e-10);
    puts("VFG_TIMING_OK display-divisors/23.976/29.97/invalid-rates/10-hour-clock/seek-reset");
    return 0;
}
