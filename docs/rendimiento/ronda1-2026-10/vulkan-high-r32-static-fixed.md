# boxel stress: static r32

- Run: 2026-10-05 15:40:17 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 2.14 -> 1.91.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 0.72 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 72425 | 3621 | 0.25 | 0.54 | 0.69 | 2.80 | 4.35 | 816 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 0 | - | - | - |
| other | 72425 | 0.25 | 0.69 | 4.35 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 0 | 0 | 0 | 0 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.0 | 0 | 0 | 0 | 0 |
| jobs in flight | 0.0 | 0 | 0 | 0 | 0 |
| chunks generating or meshing | 0.0 | 0 | 0 | 0 | 0 |

Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 20838 chunks, 7794 meshes (11460 empty), 7794 on the GPU, 1760326 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 11766 | 3.23 | 4.35 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 43226 | 11.90 | 4.31 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 52577 | 14.48 | 4.30 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 71165 | 19.65 | 4.30 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 12 | 0.00 | 4.27 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3624 | 3623 | 4.27 | 0 | 0 | 0 | 0 | 0 |
| 1 | 3647 | 3648 | 3.80 | 0 | 0 | 0 | 0 | 0 |
| 2 | 3637 | 3637 | 3.36 | 0 | 0 | 0 | 0 | 0 |
| 3 | 3632 | 3633 | 4.35 | 0 | 0 | 0 | 0 | 0 |
| 4 | 3616 | 3616 | 3.28 | 0 | 0 | 0 | 0 | 0 |
| 5 | 3640 | 3639 | 3.83 | 0 | 0 | 0 | 0 | 0 |
| 6 | 3639 | 3639 | 3.70 | 0 | 0 | 0 | 0 | 0 |
| 7 | 3618 | 3619 | 4.14 | 0 | 0 | 0 | 0 | 0 |
| 8 | 3642 | 3641 | 4.26 | 0 | 0 | 0 | 0 | 0 |
| 9 | 3614 | 3615 | 4.01 | 0 | 0 | 0 | 0 | 0 |
| 10 | 3642 | 3641 | 3.88 | 0 | 0 | 0 | 0 | 0 |
| 11 | 3637 | 3638 | 4.31 | 0 | 0 | 0 | 0 | 0 |
| 12 | 3647 | 3647 | 3.34 | 0 | 0 | 0 | 0 | 0 |
| 13 | 3635 | 3636 | 3.82 | 0 | 0 | 0 | 0 | 0 |
| 14 | 3562 | 3562 | 4.30 | 0 | 0 | 0 | 0 | 0 |
| 15 | 3613 | 3612 | 3.56 | 0 | 0 | 0 | 0 | 0 |
| 16 | 3584 | 3585 | 3.97 | 0 | 0 | 0 | 0 | 0 |
| 17 | 3586 | 3586 | 3.80 | 0 | 0 | 0 | 0 | 0 |
| 18 | 3625 | 3616 | 4.09 | 0 | 0 | 0 | 0 | 0 |
| 19 | 3585 | 3594 | 4.30 | 0 | 0 | 0 | 0 | 0 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "static",
    "label": "after-r1",
    "radius": 32,
    "speed": 0,
    "seed": "11566621",
    "trees": true,
    "vertical": "fixed",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.715,
    "settle_chunks": 20838,
    "adapter": "NVIDIA GeForce RTX 5050 Laptop GPU",
    "backend": "Vulkan",
    "tier": "High",
    "class": "Discrete",
    "driver": "NVIDIA 595.91.07",
    "present": "immediate",
    "window": "1280x720",
    "build": "release",
    "cpu": "AMD Ryzen 7 250 w/ Radeon 780M Graphics",
    "threads": 16,
    "load_start": 2.14,
    "load_end": 1.91,
    "unix_time": 1791214817,
    "date_utc": "2026-10-05 15:40:17 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 72425,
    "seconds": 20,
    "avg_fps": 3621.2,
    "mean_ms": 0.2762,
    "p50_ms": 0.2462,
    "p95_ms": 0.54,
    "p99_ms": 0.6911,
    "p999_ms": 2.8018,
    "min_ms": 0.0699,
    "max_ms": 4.3491,
    "low_1_fps": 816.2,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": null,
    "other": {
      "frames": 72425,
      "p50_ms": 0.2462,
      "p99_ms": 0.6911,
      "max_ms": 4.3491
    }
  },
  "world": {
    "generated": 0,
    "generated_per_s": 0,
    "meshes": 0,
    "meshes_per_s": 0,
    "removed": 0,
    "removed_per_s": 0,
    "jobs": 0,
    "jobs_per_s": 0,
    "upload_quads": 0,
    "upload_quads_per_s": 0,
    "crossings": 0,
    "queued": {
      "avg": 0,
      "p95": 0,
      "max": 0,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0,
      "p95": 0,
      "max": 0,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 0,
      "p95": 0,
      "max": 0,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": null,
    "catch_up_s": 0.001,
    "final": {
      "chunks": 20838,
      "data_ready": 20838,
      "mesh_ready": 7794,
      "mesh_empty": 11460,
      "gpu_chunks": 7794,
      "quads": 1760326,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 11766,
      "t_s": 3.233,
      "ms": 4.3491,
      "crossed": false,
      "presented": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 43226,
      "t_s": 11.896,
      "ms": 4.311,
      "crossed": false,
      "presented": true,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 52577,
      "t_s": 14.477,
      "ms": 4.305,
      "crossed": false,
      "presented": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 71165,
      "t_s": 19.646,
      "ms": 4.2969,
      "crossed": false,
      "presented": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 12,
      "t_s": 0.003,
      "ms": 4.2707,
      "crossed": false,
      "presented": true,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    }
  ],
  "timeline": [
    {
      "second": 0,
      "frames": 3624,
      "fps": 3622.6,
      "worst_ms": 4.2707,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 1,
      "frames": 3647,
      "fps": 3647.5,
      "worst_ms": 3.7995,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 2,
      "frames": 3637,
      "fps": 3637,
      "worst_ms": 3.3619,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 3,
      "frames": 3632,
      "fps": 3632.7,
      "worst_ms": 4.3491,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 4,
      "frames": 3616,
      "fps": 3615.9,
      "worst_ms": 3.2793,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 5,
      "frames": 3640,
      "fps": 3639.2,
      "worst_ms": 3.8324,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 6,
      "frames": 3639,
      "fps": 3639.2,
      "worst_ms": 3.7015,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 7,
      "frames": 3618,
      "fps": 3618.6,
      "worst_ms": 4.1403,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 8,
      "frames": 3642,
      "fps": 3640.9,
      "worst_ms": 4.264,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 9,
      "frames": 3614,
      "fps": 3615,
      "worst_ms": 4.0107,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 10,
      "frames": 3642,
      "fps": 3641.1,
      "worst_ms": 3.8804,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 11,
      "frames": 3637,
      "fps": 3637.7,
      "worst_ms": 4.311,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 12,
      "frames": 3647,
      "fps": 3646.8,
      "worst_ms": 3.3427,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 13,
      "frames": 3635,
      "fps": 3635.6,
      "worst_ms": 3.8162,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 14,
      "frames": 3562,
      "fps": 3561.8,
      "worst_ms": 4.305,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 15,
      "frames": 3613,
      "fps": 3611.9,
      "worst_ms": 3.5623,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 16,
      "frames": 3584,
      "fps": 3585.1,
      "worst_ms": 3.9672,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 17,
      "frames": 3586,
      "fps": 3586.2,
      "worst_ms": 3.7992,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 18,
      "frames": 3625,
      "fps": 3615.8,
      "worst_ms": 4.0899,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 19,
      "frames": 3585,
      "fps": 3593.7,
      "worst_ms": 4.2969,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    }
  ]
}
```
