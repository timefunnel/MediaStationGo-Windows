/* VFG's display-aligned sampling clock. All timestamps stay on the source timeline. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include <math.h>
#include <limits.h>

struct vfg_plan {
    double source_fps, display_fps, target_fps;
    int display_divisor;
};

static inline bool vfg_plan_divisor(double source, double display, int divisor,
                                    struct vfg_plan *plan)
{
    if (!isfinite(source) || !isfinite(display) || source <= 0 ||
        display <= source || divisor < 1)
        return false;
    double target = display / divisor;
    if (target <= source + 1e-6)
        return false;
    *plan = (struct vfg_plan){source, display, target, divisor};
    return true;
}

/* Prefer an output rate near 2x source. Among display divisors choose the closest
 * output rate above the source, breaking ties in favour of fewer generated frames.
 * This plans cadence, not GPU throughput; arbitrary sampling may need more SDK
 * runs than fixed 2x. It never changes playback speed. */
static inline bool vfg_plan_auto(double source, double display, struct vfg_plan *plan)
{
    if (!isfinite(source) || !isfinite(display) || source <= 0 || display <= source)
        return false;
    double ideal_divisor = display / source / 2;
    if (!isfinite(ideal_divisor) || ideal_divisor >= INT_MAX)
        return false;
    int low = (int)floor(ideal_divisor), high = (int)ceil(ideal_divisor);
    if (low < 1) low = 1;
    if (high < 1) high = 1;
    double best_error = INFINITY;
    struct vfg_plan best = {0};
    for (int i = 0; i < 2; i++) {
        int divisor = i ? high : low;
        struct vfg_plan candidate;
        if (!vfg_plan_divisor(source, display, divisor, &candidate))
            continue;
        double target = candidate.target_fps;
        double error = fabs(target - 2 * source);
        if (error < best_error - 1e-6 ||
            (fabs(error - best_error) <= 1e-6 && target < best.target_fps)) {
            best = candidate;
            best_error = error;
        }
    }
    if (!best.display_divisor)
        return false;
    *plan = best;
    return true;
}

struct vfg_clock {
    double origin, fps, last_pts;
    uint64_t next_index;
};

static inline void vfg_clock_start(struct vfg_clock *clock, double origin, double fps)
{
    *clock = (struct vfg_clock){origin, fps, origin, 1};
}

static inline double vfg_clock_next(const struct vfg_clock *clock)
{
    return clock->origin + (double)clock->next_index / clock->fps;
}

static inline void vfg_clock_advance(struct vfg_clock *clock)
{
    clock->last_pts = vfg_clock_next(clock);
    clock->next_index++;
}
