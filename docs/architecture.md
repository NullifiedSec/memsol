# Architecture

## North star

memsol optimizes for effective capacity and perceived latency, not minimum memory usage. Free memory is useful only when a higher-value workload needs it; otherwise spare memory may be used for valuable working sets and cheap speculative cache.

## Safety model

The first implementation phase is observer-only. Telemetry and classification must prove trustworthy before any controller is permitted to write cgroup state, reclaim memory, freeze workloads, or influence browser tabs.

Automatic learning may choose only among explicitly safe actions. Hard protections always override learned policy. The learner must never invent new privileged actions.

## Planned layers

1. **Telemetry** — PSI, `/proc/meminfo`, cgroup v2, zram, process/application accounting.
2. **Attention** — Hyprland windows, focus, visibility, named intentful workspaces, overview and bind intent.
3. **Context and events** — notifications, application lifecycle, workspace transitions, browser events, user-visible activity.
4. **Learning** — decaying transition/event statistics, cost/benefit measurements, confidence and explainability.
5. **Policy** — human working-set protection, cheap speculative warming, reclaim ordering, nap eligibility.
6. **Controllers** — cgroup weights/protection/reclaim/freezer plus browser-native tab discard. Controllers are deliberately absent in the observer phase.
7. **UI** — Quickshell workspace overview and intentful workspace management.

## Core policy concepts

Execution state and memory residency are independent:

- execution: active, background, throttled, frozen;
- residency: hot, warm, reclaimed, deep-reclaimed;
- connectivity requirements can prevent freezing while still permitting reclaim.

Resource pressure should destroy safely reconstructible state before compressing irreplaceable state. Browser-native discarded tabs, file-backed cache, inactive app memory, and compressed anonymous state form progressively more expensive tiers.

## Human working set

memsol should learn the small cluster of applications/windows/tabs that form the user's current task. Supporting workloads such as a dev server or preview browser remain valuable even when unfocused. Workspaces and event history provide context for predicting what should stay warm next.

## Explainability

Every future automatic action must be inspectable: what signal caused it, what benefit was expected, what cost was observed, and which hard safety constraints applied.
