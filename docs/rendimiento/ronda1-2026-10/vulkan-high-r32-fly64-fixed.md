# boxel stress: fly r32 64 vox/s

- Run: 2026-10-05 15:39:31 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 2.70 -> 2.13.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 0.71 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 73849 | 3692 | 0.24 | 0.53 | 0.69 | 2.57 | 5.24 | 792 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 1.63 | 4.22 | 4.22 |
| other | 73809 | 0.24 | 0.68 | 5.24 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 804 | 318 | 280 | 1391 | 70074 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 1.3 | 0 | 354 | 0 | 0 |
| jobs in flight | 0.6 | 0 | 144 | 0 | 0 |
| chunks generating or meshing | 3.6 | 0 | 719 | 0 | 0 |

After a chunk border the world finished the new chunks in 9 ms on average, 13 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 21870 chunks, 8549 meshes (12008 empty), 8549 on the GPU, 1881685 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 61280 | 16.67 | 5.24 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 50553 | 13.75 | 4.70 | no | no | 46 | 5 | 0 | 1442 | 166 | 60 |
| 20198 | 5.62 | 4.23 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 39341 | 10.75 | 4.22 | yes | no | 60 | 0 | 149 | 0 | 342 | 60 |
| 71563 | 19.39 | 4.17 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3585 | 3585 | 3.97 | 804 | 316 | 2 | 348 | 84 |
| 1 | 3600 | 3589 | 3.70 | 804 | 316 | 2 | 342 | 96 |
| 2 | 3551 | 3562 | 3.66 | 804 | 310 | 2 | 342 | 102 |
| 3 | 3578 | 3577 | 3.81 | 804 | 316 | 2 | 336 | 84 |
| 4 | 3611 | 3612 | 4.12 | 804 | 301 | 2 | 336 | 84 |
| 5 | 3652 | 3652 | 4.23 | 804 | 300 | 2 | 336 | 102 |
| 6 | 3716 | 3716 | 4.01 | 804 | 303 | 2 | 336 | 72 |
| 7 | 3705 | 3705 | 3.73 | 804 | 321 | 2 | 348 | 96 |
| 8 | 3745 | 3744 | 3.85 | 804 | 328 | 2 | 342 | 93 |
| 9 | 3764 | 3764 | 4.11 | 804 | 334 | 2 | 348 | 72 |
| 10 | 3760 | 3760 | 4.22 | 804 | 318 | 2 | 354 | 120 |
| 11 | 3738 | 3738 | 3.40 | 804 | 310 | 2 | 306 | 114 |
| 12 | 3724 | 3724 | 3.35 | 804 | 320 | 2 | 336 | 120 |
| 13 | 3722 | 3722 | 4.70 | 804 | 308 | 2 | 348 | 144 |
| 14 | 3669 | 3669 | 3.81 | 804 | 326 | 2 | 286 | 84 |
| 15 | 3668 | 3668 | 3.57 | 804 | 335 | 2 | 348 | 84 |
| 16 | 3722 | 3722 | 5.24 | 804 | 329 | 2 | 342 | 78 |
| 17 | 3772 | 3772 | 3.58 | 804 | 326 | 2 | 354 | 78 |
| 18 | 3814 | 3812 | 3.14 | 804 | 330 | 2 | 354 | 72 |
| 19 | 3753 | 3755 | 4.17 | 804 | 318 | 2 | 294 | 102 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "after-r1",
    "radius": 32,
    "speed": 64,
    "seed": "11566621",
    "trees": true,
    "vertical": "fixed",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.71,
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
    "load_start": 2.7,
    "load_end": 2.13,
    "unix_time": 1791214771,
    "date_utc": "2026-10-05 15:39:31 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 73849,
    "seconds": 20,
    "avg_fps": 3692.4,
    "mean_ms": 0.2708,
    "p50_ms": 0.2404,
    "p95_ms": 0.5285,
    "p99_ms": 0.6861,
    "p999_ms": 2.575,
    "min_ms": 0.0726,
    "max_ms": 5.2419,
    "low_1_fps": 792.4,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 1.6256,
      "p99_ms": 4.2221,
      "max_ms": 4.2221
    },
    "other": {
      "frames": 73809,
      "p50_ms": 0.2403,
      "p99_ms": 0.6827,
      "max_ms": 5.2419
    }
  },
  "world": {
    "generated": 16080,
    "generated_per_s": 804,
    "meshes": 6365,
    "meshes_per_s": 318.2,
    "removed": 5610,
    "removed_per_s": 280.5,
    "jobs": 27823,
    "jobs_per_s": 1391.1,
    "upload_quads": 1401476,
    "upload_quads_per_s": 70073.8,
    "crossings": 40,
    "queued": {
      "avg": 1.3,
      "p95": 0,
      "max": 354,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.6,
      "p95": 0,
      "max": 144,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 3.6,
      "p95": 0,
      "max": 719,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 40,
      "finished": 40,
      "cut_off": 0,
      "avg_ms": 9.4816,
      "max_ms": 12.7448
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 21870,
      "data_ready": 21870,
      "mesh_ready": 8549,
      "mesh_empty": 12008,
      "gpu_chunks": 8549,
      "quads": 1881685,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 61280,
      "t_s": 16.665,
      "ms": 5.2419,
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
      "frame": 50553,
      "t_s": 13.754,
      "ms": 4.6973,
      "crossed": false,
      "presented": false,
      "generated": 46,
      "meshes": 5,
      "removed": 0,
      "upload_quads": 1442,
      "queued": 166,
      "in_flight": 60
    },
    {
      "frame": 20198,
      "t_s": 5.62,
      "ms": 4.2331,
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
      "frame": 39341,
      "t_s": 10.75,
      "ms": 4.2221,
      "crossed": true,
      "presented": false,
      "generated": 60,
      "meshes": 0,
      "removed": 149,
      "upload_quads": 0,
      "queued": 342,
      "in_flight": 60
    },
    {
      "frame": 71563,
      "t_s": 19.388,
      "ms": 4.171,
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
      "frames": 3585,
      "fps": 3584.7,
      "worst_ms": 3.9696,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 348,
      "in_flight_max": 84
    },
    {
      "second": 1,
      "frames": 3600,
      "fps": 3588.9,
      "worst_ms": 3.6957,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 342,
      "in_flight_max": 96
    },
    {
      "second": 2,
      "frames": 3551,
      "fps": 3562.1,
      "worst_ms": 3.6641,
      "generated": 804,
      "meshes": 310,
      "crossings": 2,
      "queued_max": 342,
      "in_flight_max": 102
    },
    {
      "second": 3,
      "frames": 3578,
      "fps": 3576.8,
      "worst_ms": 3.8111,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 336,
      "in_flight_max": 84
    },
    {
      "second": 4,
      "frames": 3611,
      "fps": 3612.2,
      "worst_ms": 4.123,
      "generated": 804,
      "meshes": 301,
      "crossings": 2,
      "queued_max": 336,
      "in_flight_max": 84
    },
    {
      "second": 5,
      "frames": 3652,
      "fps": 3651.9,
      "worst_ms": 4.2331,
      "generated": 804,
      "meshes": 300,
      "crossings": 2,
      "queued_max": 336,
      "in_flight_max": 102
    },
    {
      "second": 6,
      "frames": 3716,
      "fps": 3716,
      "worst_ms": 4.0088,
      "generated": 804,
      "meshes": 303,
      "crossings": 2,
      "queued_max": 336,
      "in_flight_max": 72
    },
    {
      "second": 7,
      "frames": 3705,
      "fps": 3705.1,
      "worst_ms": 3.7271,
      "generated": 804,
      "meshes": 321,
      "crossings": 2,
      "queued_max": 348,
      "in_flight_max": 96
    },
    {
      "second": 8,
      "frames": 3745,
      "fps": 3744.2,
      "worst_ms": 3.8548,
      "generated": 804,
      "meshes": 328,
      "crossings": 2,
      "queued_max": 342,
      "in_flight_max": 93
    },
    {
      "second": 9,
      "frames": 3764,
      "fps": 3764.3,
      "worst_ms": 4.109,
      "generated": 804,
      "meshes": 334,
      "crossings": 2,
      "queued_max": 348,
      "in_flight_max": 72
    },
    {
      "second": 10,
      "frames": 3760,
      "fps": 3760.2,
      "worst_ms": 4.2221,
      "generated": 804,
      "meshes": 318,
      "crossings": 2,
      "queued_max": 354,
      "in_flight_max": 120
    },
    {
      "second": 11,
      "frames": 3738,
      "fps": 3738.4,
      "worst_ms": 3.4015,
      "generated": 804,
      "meshes": 310,
      "crossings": 2,
      "queued_max": 306,
      "in_flight_max": 114
    },
    {
      "second": 12,
      "frames": 3724,
      "fps": 3723.8,
      "worst_ms": 3.3509,
      "generated": 804,
      "meshes": 320,
      "crossings": 2,
      "queued_max": 336,
      "in_flight_max": 120
    },
    {
      "second": 13,
      "frames": 3722,
      "fps": 3722,
      "worst_ms": 4.6973,
      "generated": 804,
      "meshes": 308,
      "crossings": 2,
      "queued_max": 348,
      "in_flight_max": 144
    },
    {
      "second": 14,
      "frames": 3669,
      "fps": 3669,
      "worst_ms": 3.8133,
      "generated": 804,
      "meshes": 326,
      "crossings": 2,
      "queued_max": 286,
      "in_flight_max": 84
    },
    {
      "second": 15,
      "frames": 3668,
      "fps": 3668,
      "worst_ms": 3.5736,
      "generated": 804,
      "meshes": 335,
      "crossings": 2,
      "queued_max": 348,
      "in_flight_max": 84
    },
    {
      "second": 16,
      "frames": 3722,
      "fps": 3721.6,
      "worst_ms": 5.2419,
      "generated": 804,
      "meshes": 329,
      "crossings": 2,
      "queued_max": 342,
      "in_flight_max": 78
    },
    {
      "second": 17,
      "frames": 3772,
      "fps": 3772.3,
      "worst_ms": 3.5823,
      "generated": 804,
      "meshes": 326,
      "crossings": 2,
      "queued_max": 354,
      "in_flight_max": 78
    },
    {
      "second": 18,
      "frames": 3814,
      "fps": 3812.5,
      "worst_ms": 3.1396,
      "generated": 804,
      "meshes": 330,
      "crossings": 2,
      "queued_max": 354,
      "in_flight_max": 72
    },
    {
      "second": 19,
      "frames": 3753,
      "fps": 3754.9,
      "worst_ms": 4.171,
      "generated": 804,
      "meshes": 318,
      "crossings": 2,
      "queued_max": 294,
      "in_flight_max": 102
    }
  ]
}
```
