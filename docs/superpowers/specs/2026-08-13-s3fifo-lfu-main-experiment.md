# S3-FIFO main: LFU-under-memory-pressure experiment

**Date:** 2026-08-13
**Status:** Follow-up — not part of Engine v1
**Depends on:** stock `CacheStore` S3-FIFO (`src/proxy/cache.rs`)
**Related:** `docs/superpowers/specs/2026-08-13-engine-library-design.md`

## Problem

Under `max_memory`, after cold **small** is gone, **main** evicts FIFO + second chance (`freq == 0` drop, else requeue and reset `freq`). That is not “lowest frequency first.” The hypothesis: when RAM is tight and the working set is Zipf (agentic repeats), FIFO-on-main evicts a still-hot key that sat at the head, and hit rate suffers.

## Non-goals

- Do not change this in Engine v1.
- Do not treat the current 0..=3 reset counter as LFU.
- Do not change `max_entries` eviction in the first experiment (FIFO main stays for count pressure).
- Do not drop ghost or small.

## Experiment

1. Add a decaying hit count on `CacheEntry` (not the 2-bit `freq` used for second chance).
2. In the **`max_memory` loop only**, after small cannot free more values, evict the **lowest decaying count** from main instead of FIFO-head.
3. Keep `max_entries` on current S3-FIFO.
4. Compare with `make bench-hitrate` (and a full-cache / low-`max_memory` variant): exact HR, false-hit gate unchanged, eviction counts.
5. Ship only if HR improves enough to justify the extra bookkeeping (scan or heap on the admit path). Fail the experiment if it is noise or hurts scan resistance.

## Success

A measured hit-rate win on a full cache, no regression on scan/one-hit-wonder traces, no change to the ~0.1 ms hit path (ranking is admit/evict only).

## Note

`freq` today is 0..=3 and zeroed on check. Ranking on that signal is not this plan.
