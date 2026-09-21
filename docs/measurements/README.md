# Performance summaries

This directory contains reviewed aggregates for the [performance report](../performance-running-app.md):

- [Windows 10 Release, 2026-09-21](windows10-release-20260921-summary.json)
- [Windows 11 Release and resource probe, 2026-09-20](windows11-release-20260920-summary.json)

Commit the report, measurement tools, and compact summaries. Save raw samples, per-cycle timings, waiting observations, failed attempts, and temporary captures under the ignored `logs/` directory. Raw JSON files are also ignored here to prevent accidental additions.

Each run records the original filename and SHA256, measurement scope, aggregate results, and completion status. The original files are retained locally under `logs/measurements/`; they are not included in the repository or distribution packages. Summaries exclude machine names, absolute local paths, process/window identifiers, and mouse coordinates. Failed attempts remain listed with the reason they were excluded from the results.

Resource arrays use the order `first, last, minimum, maximum`; memory values are bytes. CPU, timing, resource counts, and measurement limits are explained in the performance report. The standalone resource probe retains its own checkpoints and timing definition, separate from measurements of the running application.
