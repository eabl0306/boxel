# boxel stress: static r32

- Run: 2026-10-05 15:40:40 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 1.91 -> 1.65.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 0.70 s with 65987 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 72609 | 3630 | 0.25 | 0.53 | 0.68 | 2.66 | 4.31 | 848 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 0 | - | - | - |
| other | 72609 | 0.25 | 0.68 | 4.31 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 0 | 0 | 0 | 0 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.0 | 0 | 0 | 0 | 0 |
| jobs in flight | 0.0 | 0 | 0 | 0 | 0 |
| chunks generating or meshing | 0.0 | 0 | 0 | 0 | 0 |

Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 65987 chunks, 7794 meshes (46759 empty), 7794 on the GPU, 1760326 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 37653 | 10.38 | 4.31 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 40903 | 11.27 | 4.29 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 60528 | 16.67 | 4.15 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 65913 | 18.16 | 4.09 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 31597 | 8.71 | 4.02 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3629 | 3628 | 3.35 | 0 | 0 | 0 | 0 | 0 |
| 1 | 3648 | 3647 | 3.59 | 0 | 0 | 0 | 0 | 0 |
| 2 | 3607 | 3607 | 3.63 | 0 | 0 | 0 | 0 | 0 |
| 3 | 3634 | 3632 | 2.69 | 0 | 0 | 0 | 0 | 0 |
| 4 | 3633 | 3635 | 3.49 | 0 | 0 | 0 | 0 | 0 |
| 5 | 3628 | 3628 | 3.79 | 0 | 0 | 0 | 0 | 0 |
| 6 | 3620 | 3619 | 3.57 | 0 | 0 | 0 | 0 | 0 |
| 7 | 3619 | 3620 | 3.33 | 0 | 0 | 0 | 0 | 0 |
| 8 | 3615 | 3615 | 4.02 | 0 | 0 | 0 | 0 | 0 |
| 9 | 3638 | 3637 | 3.78 | 0 | 0 | 0 | 0 | 0 |
| 10 | 3653 | 3654 | 4.31 | 0 | 0 | 0 | 0 | 0 |
| 11 | 3600 | 3600 | 4.29 | 0 | 0 | 0 | 0 | 0 |
| 12 | 3636 | 3635 | 3.40 | 0 | 0 | 0 | 0 | 0 |
| 13 | 3628 | 3627 | 3.59 | 0 | 0 | 0 | 0 | 0 |
| 14 | 3630 | 3631 | 3.69 | 0 | 0 | 0 | 0 | 0 |
| 15 | 3651 | 3651 | 2.90 | 0 | 0 | 0 | 0 | 0 |
| 16 | 3650 | 3648 | 4.15 | 0 | 0 | 0 | 0 | 0 |
| 17 | 3634 | 3636 | 3.10 | 0 | 0 | 0 | 0 | 0 |
| 18 | 3634 | 3634 | 4.09 | 0 | 0 | 0 | 0 | 0 |
| 19 | 3622 | 3622 | 3.32 | 0 | 0 | 0 | 0 | 0 |

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
    "vertical": "8",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.701,
    "settle_chunks": 65987,
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
    "load_start": 1.91,
    "load_end": 1.65,
    "unix_time": 1791214840,
    "date_utc": "2026-10-05 15:40:40 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 72609,
    "seconds": 20,
    "avg_fps": 3630.4,
    "mean_ms": 0.2754,
    "p50_ms": 0.2464,
    "p95_ms": 0.5335,
    "p99_ms": 0.6811,
    "p999_ms": 2.6598,
    "min_ms": 0.0721,
    "max_ms": 4.3073,
    "low_1_fps": 847.6,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": null,
    "other": {
      "frames": 72609,
      "p50_ms": 0.2464,
      "p99_ms": 0.6811,
      "max_ms": 4.3073
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
      "chunks": 65987,
      "data_ready": 65987,
      "mesh_ready": 7794,
      "mesh_empty": 46759,
      "gpu_chunks": 7794,
      "quads": 1760326,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 37653,
      "t_s": 10.376,
      "ms": 4.3073,
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
      "frame": 40903,
      "t_s": 11.268,
      "ms": 4.289,
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
      "frame": 60528,
      "t_s": 16.672,
      "ms": 4.15,
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
      "frame": 65913,
      "t_s": 18.155,
      "ms": 4.0919,
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
      "frame": 31597,
      "t_s": 8.711,
      "ms": 4.0173,
      "crossed": false,
      "presented": false,
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
      "frames": 3629,
      "fps": 3628.4,
      "worst_ms": 3.3549,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 1,
      "frames": 3648,
      "fps": 3647.5,
      "worst_ms": 3.5888,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 2,
      "frames": 3607,
      "fps": 3607.5,
      "worst_ms": 3.6309,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 3,
      "frames": 3634,
      "fps": 3632.5,
      "worst_ms": 2.6857,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 4,
      "frames": 3633,
      "fps": 3634.8,
      "worst_ms": 3.4897,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 5,
      "frames": 3628,
      "fps": 3628.1,
      "worst_ms": 3.7875,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 6,
      "frames": 3620,
      "fps": 3619,
      "worst_ms": 3.5724,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 7,
      "frames": 3619,
      "fps": 3619.8,
      "worst_ms": 3.3274,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 8,
      "frames": 3615,
      "fps": 3615.1,
      "worst_ms": 4.0173,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 9,
      "frames": 3638,
      "fps": 3637.5,
      "worst_ms": 3.7827,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 10,
      "frames": 3653,
      "fps": 3653.5,
      "worst_ms": 4.3073,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 11,
      "frames": 3600,
      "fps": 3600.2,
      "worst_ms": 4.289,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 12,
      "frames": 3636,
      "fps": 3635.2,
      "worst_ms": 3.4047,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 13,
      "frames": 3628,
      "fps": 3627.4,
      "worst_ms": 3.5859,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 14,
      "frames": 3630,
      "fps": 3631.3,
      "worst_ms": 3.6873,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 15,
      "frames": 3651,
      "fps": 3651.1,
      "worst_ms": 2.898,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 16,
      "frames": 3650,
      "fps": 3648,
      "worst_ms": 4.15,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 17,
      "frames": 3634,
      "fps": 3635.9,
      "worst_ms": 3.1038,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 18,
      "frames": 3634,
      "fps": 3633.9,
      "worst_ms": 4.0919,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 19,
      "frames": 3622,
      "fps": 3621.6,
      "worst_ms": 3.3207,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    }
  ]
}
```
