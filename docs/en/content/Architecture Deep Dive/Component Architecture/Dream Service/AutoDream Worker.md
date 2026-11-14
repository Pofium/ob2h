# AutoDream Worker

<cite>
**Referenced Files in This Document**
- [autodream.rs](file://src/dream/autodream.rs)
- [dream_mod.rs](file://src/dream/mod.rs)
- [config.rs](file://src/config.rs)
- [lib.rs](file://src/lib.rs)
</cite>

## Table of Contents
1. [Introduction](#introduction)
2. [Project Structure](#project-structure)
3. [Core Components](#core-components)
4. [Architecture Overview](#architecture-overview)
5. [Detailed Component Analysis](#detailed-component-analysis)
6. [Dependency Analysis](#dependency-analysis)
7. [Performance Considerations](#performance-considerations)
8. [Troubleshooting Guide](#troubleshooting-guide)
9. [Conclusion](#conclusion)

## Introduction
AutoDream Worker is the background component that automatically schedules and triggers nightly dream processing for OB2H. It periodically checks whether enough new daily events have occurred, safely acquires a file-based lock, runs the Dream pipeline, optionally performs a bench gate evaluation, and then executes scheduled maintenance tasks such as memory decay, purge, lessons decay sweep, audit ledger rotation, optional slot reflection, daily log pruning, and after-dream synchronization with peers.

The worker is integrated into the main application lifecycle through the shared `AppContext`, which constructs all services (workspace, memory, graph, Ralph, sync, Git store, database, LLM client) and passes them to both the Dream engine and AutoDream Worker. Configuration is driven by environment variables prefixed with `OB2H_`.

## Project Structure
AutoDream Worker lives under the `dream` module alongside the core Dream engine and related utilities:

```mermaid
graph TB
lib_rs["src/lib.rs"] --> dream_mod_rs["src/dream/mod.rs"]
dream_mod_rs --> autodream_rs["src/dream/autodream.rs"]
config_rs["src/config.rs"] --> autodream_rs
config_rs --> dream_mod_rs
lib_rs --> config_rs
```

**Diagram sources**
- [lib.rs:34-34](file://src/lib.rs#L34-L34)
- [dream_mod.rs:1-6](file://src/dream/mod.rs#L1-L6)
- [autodream.rs:1-16](file://src/dream/autodream.rs#L1-L16)
- [config.rs:1-106](file://src/config.rs#L1-L106)

**Section sources**
- [lib.rs:1-25](file://src/lib.rs#L1-L25)
- [dream_mod.rs:1-21](file://src/dream/mod.rs#L1-L21)
- [autodream.rs:1-16](file://src/dream/autodream.rs#L1-L16)
- [config.rs:1-106](file://src/config.rs#L1-L106)

## Core Components
- **AutoDreamWorker**: Background Tokio task that ticks on a configured interval, evaluates gating conditions, runs Dream, and performs post-run maintenance.
- **Dream**: The core dream processing engine that reads history, performs analysis and edits, extracts graph entities, revises memory trust, consolidates memory, revises Ralph findings, commits changes, and records run statistics.
- **Settings**: Central configuration object loaded from environment variables, including autodream toggles, intervals, thresholds, and feature flags.
- **AppContext integration**: Application initialization builds services and starts background workers conditionally based on settings.

Key responsibilities:
- Scheduling: periodic tick using Tokio interval.
- Gating: minimum time since last run and minimum number of new daily events.
- Concurrency safety: file-based lock with staleness detection.
- Execution: invoke Dream.run with trigger "auto".
- Post-processing: bench gate, memory maintenance, lessons decay, audit rotation, optional slot reflection, daily pruning, peer sync.

**Section sources**
- [autodream.rs:20-59](file://src/dream/autodream.rs#L20-L59)
- [dream_mod.rs:110-143](file://src/dream/mod.rs#L110-L143)
- [config.rs:8-106](file://src/config.rs#L8-L106)
- [lib.rs:46-134](file://src/lib.rs#L46-L134)

## Architecture Overview
AutoDream Worker integrates with the main application lifecycle and orchestrates Dream execution while coordinating with memory, lessons, audit, workspace, Git store, and sync components.

```mermaid
sequenceDiagram
participant App as "Application"
participant Lib as "lib.rs"
participant Worker as "AutoDreamWorker"
participant Dream as "Dream"
participant Memory as "MemoryService"
participant Lessons as "LessonsService"
participant Audit as "Audit"
participant Sync as "SyncManager"
participant Git as "GitStore"
participant DB as "Database"
App->>Lib : init_app(settings)
Lib-->>App : AppContext with services
App->>Lib : start_background_workers(ctx)
Lib->>Worker : new(...) + start()
loop every interval
Worker->>Worker : should_run()
alt sufficient events and min interval
Worker->>Worker : acquire_lock()
Worker->>Git : head()
Worker->>Dream : run("auto")
Dream->>DB : insert dream_runs
Dream-->>Worker : DreamStats
opt success
Worker->>Dream : append_run_gate(run_id, verdict)
end
Worker->>Memory : decay_importance()
Worker->>Memory : purge_weak()
Worker->>Memory : purge_tombstones()
Worker->>Lessons : decay_sweep("dream")
Worker->>Audit : prune_weekly(90)
opt OB2H_DREAM_SLOTS=1
Worker->>Workspace : dream_reflect()
end
Worker->>Worker : prune_daily()
Worker->>Worker : release_lock()
opt sync available
Worker->>Sync : run_scheduled()
end
else insufficient events or too soon
Worker->>Worker : continue
end
end
```

**Diagram sources**
- [lib.rs:137-161](file://src/lib.rs#L137-L161)
- [autodream.rs:61-145](file://src/dream/autodream.rs#L61-L145)
- [dream_mod.rs:145-181](file://src/dream/mod.rs#L145-L181)
- [dream_mod.rs:196-288](file://src/dream/mod.rs#L196-L288)

## Detailed Component Analysis

### AutoDreamWorker Lifecycle
AutoDreamWorker is constructed with references to Dream, Workspace, MemoryService, LessonsService, Settings, optional SyncManager, GitStore, and Database. Its `start` method spawns a Tokio task that:
- Checks if autodream is enabled.
- Creates a ticker with an interval derived from `autodream_interval_min`, clamped to at least 60 seconds.
- On each tick, evaluates `should_run()` and skips if conditions are not met.
- Acquires a file-based lock; if stale, warns and proceeds.
- Records Git HEAD before Dream execution.
- Runs Dream with trigger "auto".
- On success, optionally runs bench gate and appends verdict to Dream stats.
- Saves last run timestamp.
- Performs memory decay, purge weak entries, purge tombstones beyond retention.
- Runs weekly lessons decay sweep.
- Optionally runs weekly audit ledger pruning.
- Optionally reflects daily patterns into slots when `OB2H_DREAM_SLOTS` is set.
- Prunes daily logs older than retention.
- Releases lock.
- Attempts after-dream sync if SyncManager is present.

```mermaid
flowchart TD
Start(["Worker Loop"]) --> Tick["Wait for interval tick"]
Tick --> ShouldRun{"should_run()?"}
ShouldRun --> |No| Skip["Log skip reason<br/>Continue loop"]
ShouldRun --> |Yes| Lock{"acquire_lock()?"}
Lock --> |No| Skip
Lock --> |Yes| Head["Record Git HEAD"]
Head --> RunDream["Dream.run('auto')"]
RunDream --> Result{"Result OK?"}
Result --> |Err| Warn["Warn error"]
Result --> |Ok| Gate{"bench_gate_enabled?"}
Gate --> |Yes| Bench["run_gate()"]
Bench --> Append["append_run_gate()"]
Gate --> |No| SaveLast["save_last_run()"]
Append --> SaveLast
SaveLast --> Maintenance["memory.decay/purge/tombstones"]
Maintenance --> Lessons["lessons.decay_sweep('dream')"]
Lessons --> Audit["audit.prune_weekly(90)"]
Audit --> Slots{"OB2H_DREAM_SLOTS=1?"}
Slots --> |Yes| Reflect["lifecycle.slots.dream_reflect()"]
Slots --> |No| PruneDaily["prune_daily()"]
Reflect --> PruneDaily
PruneDaily --> Release["release_lock()"]
Release --> Sync{"sync available?"}
Sync --> |Yes| AfterSync["sync.run_scheduled()"]
Sync --> |No| EndLoop["End iteration"]
AfterSync --> EndLoop
EndLoop --> Tick
Warn --> SaveLast
```

**Diagram sources**
- [autodream.rs:61-145](file://src/dream/autodream.rs#L61-L145)
- [autodream.rs:169-224](file://src/dream/autodream.rs#L169-L224)
- [autodream.rs:226-271](file://src/dream/autodream.rs#L226-L271)

**Section sources**
- [autodream.rs:20-59](file://src/dream/autodream.rs#L20-L59)
- [autodream.rs:61-145](file://src/dream/autodream.rs#L61-L145)
- [autodream.rs:169-271](file://src/dream/autodream.rs#L169-L271)

### Scheduling Mechanisms
Scheduling is driven by three primary mechanisms:
- **Interval ticker**: Uses `tokio::time::interval` with duration computed from `autodream_interval_min`, ensuring at least 60 seconds between ticks.
- **Minimum interval gate**: Enforces a minimum gap since the last successful run using `autodream_min_interval_h`.
- **Event threshold gate**: Counts new daily JSONL events since the last run timestamp and requires at least `autodream_min_events`.

The event counting logic scans the daily directory for `.jsonl` files, parses lines, and compares timestamps against the last run ISO string. If no last run exists, it counts all events.

```mermaid
flowchart TD
CountStart["count_daily_events_since(since_iso)"] --> DailyDir{"daily_dir exists?"}
DailyDir --> |No| Zero["Return 0"]
DailyDir --> |Yes| ReadEntries["Read directory entries"]
ReadEntries --> ForEach["For each entry"]
ForEach --> IsJsonl{"extension == 'jsonl'?"}
IsJsonl --> |No| NextEntry["Next entry"]
IsJsonl --> |Yes| ReadFile["Read file content"]
ReadFile --> ForLine["For each line"]
ForLine --> Empty{"line empty?"}
Empty --> |Yes| NextLine["Next line"]
Empty --> |No| Parse{"parse JSON"}
Parse --> SinceEmpty{"since_iso empty?"}
SinceEmpty --> |Yes| Increment["count += 1"]
SinceEmpty --> |No| Compare{"timestamp > since_iso?"}
Compare --> |Yes| Increment
Compare --> |No| NextLine
Increment --> NextLine
NextLine --> NextEntry
NextEntry --> Done["Return count"]
```

**Diagram sources**
- [autodream.rs:194-224](file://src/dream/autodream.rs#L194-L224)

**Section sources**
- [autodream.rs:61-76](file://src/dream/autodream.rs#L61-L76)
- [autodream.rs:169-192](file://src/dream/autodream.rs#L169-L192)
- [autodream.rs:194-224](file://src/dream/autodream.rs#L194-L224)

### Integration with System Events
AutoDream Worker coordinates with several system components:
- **MemoryService**: Decay importance, purge weak entries, purge tombstones based on retention days.
- **LessonsService**: Weekly decay sweep with cursor tracking and candidate-for-forget reporting without deletion.
- **Audit Ledger**: Weekly pruning of audit entries older than 90 days.
- **Slots Service**: Optional reflection of daily patterns into slots when environment flag is set.
- **SyncManager**: Best-effort after-dream synchronization with peers.
- **GitStore**: Record HEAD before Dream execution for potential rollback reference.
- **Database**: Persist Dream run stats and support bench gate verdict appending.

**Section sources**
- [autodream.rs:114-142](file://src/dream/autodream.rs#L114-L142)
- [dream_mod.rs:545-566](file://src/dream/mod.rs#L545-L566)

### Error Handling and Resilience
- **Lock staleness**: If a lock file exists but is older than `LOCK_STALE_SECS` (3600 seconds), the worker warns and takes over the lock.
- **Dream errors**: Errors during Dream execution are logged and result in a failed run status recorded in the database.
- **Bench gate failures**: Bench gate execution is best-effort; failures do not prevent Dream completion.
- **Maintenance operations**: Memory decay, purge, lessons decay, audit pruning, and slot reflection are wrapped in best-effort calls that ignore errors to avoid disrupting the main flow.
- **After-dream sync**: Sync failures are warned but do not fail the Dream run.

**Section sources**
- [autodream.rs:226-247](file://src/dream/autodream.rs#L226-L247)
- [autodream.rs:86-110](file://src/dream/autodream.rs#L86-L110)
- [dream_mod.rs:155-181](file://src/dream/mod.rs#L155-L181)

## Dependency Analysis
AutoDream Worker depends on multiple subsystems, creating a cohesive integration layer between scheduling, data persistence, memory management, and external coordination.

```mermaid
classDiagram
class AutoDreamWorker {
+dream : Arc<Dream>
+workspace : Arc<Workspace>
+memory : Arc<MemoryService>
+lessons : Arc<LessonsService>
+settings : Settings
+sync : Option<Arc<SyncManager>>
+gitstore : Arc<GitStore>
+db : Database
+running : Arc<Mutex<bool>>
+start()
+should_run() (bool, String)
+prune_daily() usize
-acquire_lock() bool
-release_lock() void
-save_last_run() void
-last_run_iso() Option<String>
-count_daily_events_since(since_iso) usize
}
class Dream {
+run(trigger) DreamStats
+append_run_gate(run_id, verdict)
-execute_dream(run_id) DreamStats
-phase1(records) String
-phase2(analysis) Vec<Value>
-extract_to_graph(records) (usize, usize)
-revise_memory() Vec<Value>
-revise_ralph_findings() Vec<Value>
-consolidate_memory() Value
-finish_run(run_id, status, stats)
}
class Settings {
+autodream_enabled : bool
+autodream_interval_min : u64
+autodream_min_interval_h : i64
+autodream_min_events : usize
+retention_days : i64
+dream_extract_enabled : bool
+dream_memory_revision : bool
+dream_belief : bool
+dream_ralph_revision : bool
+bench_gate_enabled : bool
+bench_gate_timeout_ms : u64
}
class MemoryService
class LessonsService
class SyncManager
class GitStore
class Database
AutoDreamWorker --> Dream : "executes"
AutoDreamWorker --> MemoryService : "maintenance"
AutoDreamWorker --> LessonsService : "decay sweep"
AutoDreamWorker --> SyncManager : "after-dream sync"
AutoDreamWorker --> GitStore : "record HEAD"
AutoDreamWorker --> Database : "persist stats"
AutoDreamWorker --> Settings : "configuration"
```

**Diagram sources**
- [autodream.rs:20-59](file://src/dream/autodream.rs#L20-L59)
- [dream_mod.rs:110-143](file://src/dream/mod.rs#L110-L143)
- [config.rs:8-106](file://src/config.rs#L8-L106)

**Section sources**
- [autodream.rs:20-59](file://src/dream/autodream.rs#L20-L59)
- [dream_mod.rs:110-143](file://src/dream/mod.rs#L110-L143)
- [config.rs:8-106](file://src/config.rs#L8-L106)

## Performance Considerations
- **Interval tuning**: `autodream_interval_min` controls how frequently the worker checks for work. Lower values increase CPU usage but reduce latency; higher values reduce overhead but may delay processing.
- **Event threshold**: `autodream_min_events` prevents unnecessary Dream runs when few new events exist, reducing LLM calls and I/O.
- **Minimum interval**: `autodream_min_interval_h` ensures sufficient time between runs, preventing excessive processing.
- **Batch size**: `dream_batch` controls how many history records are processed per Dream run, balancing throughput and context window limits.
- **Memory maintenance**: Decay and purge operations are best-effort and designed to be lightweight; they run after each successful Dream to keep memory healthy without blocking the main flow.
- **Bench gate**: When enabled, adds overhead due to quick benchmark execution but provides valuable feedback on Dream effectiveness.

[No sources needed since this section provides general guidance]

## Troubleshooting Guide
Common issues and resolutions:
- **Autodream not running**: Check `OB2H_AUTODREAM_ENABLED`; ensure it is not set to `0` or `false`.
- **Too frequent or infrequent runs**: Adjust `OB2H_AUTODREAM_INTERVAL_MIN`, `OB2H_AUTODREAM_MIN_INTERVAL_H`, and `OB2H_AUTODREAM_MIN_EVENTS`.
- **Lock conflicts**: Verify that `autodream.lock` is not stale; the worker will warn and take over locks older than 1 hour.
- **No events detected**: Ensure daily JSONL files exist in the workspace daily directory and contain valid JSON lines with timestamps.
- **Dream failures**: Check database `dream_runs` table for error messages and review logs for LLM or I/O errors.
- **Memory not decaying**: Verify that MemoryService is properly initialized and that retention settings are correct.
- **Lessons not decaying**: Ensure LessonsService is available and that `OB2H_DREAM_SLOTS` is not interfering with expected behavior.
- **Sync not running**: Confirm SyncManager is provided and `OB2H_AUTOSYNC_ENABLED` is set appropriately.

**Section sources**
- [config.rs:225-239](file://src/config.rs#L225-L239)
- [autodream.rs:61-76](file://src/dream/autodream.rs#L61-L76)
- [autodream.rs:226-247](file://src/dream/autodream.rs#L226-L247)
- [dream_mod.rs:155-181](file://src/dream/mod.rs#L155-L181)

## Conclusion
AutoDream Worker provides robust automated scheduling and triggering of dream processing within OB2H. It combines configurable intervals, event-based gating, file locking, and comprehensive post-run maintenance to ensure reliable, efficient, and resilient operation. Integration with the main application lifecycle ensures seamless coordination with memory, lessons, audit, sync, and other subsystems. Proper configuration of environment variables allows fine-tuning of behavior to match operational requirements, while best-effort error handling maintains stability even when individual components fail.

[No sources needed since this section summarizes without analyzing specific files]