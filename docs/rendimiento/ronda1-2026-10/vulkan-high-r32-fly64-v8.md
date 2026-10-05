# boxel stress: fly r32 64 vox/s

- Run: 2026-10-05 15:39:54 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 2.13 -> 2.24.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 0.71 s with 65987 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 73118 | 3656 | 0.24 | 0.53 | 0.71 | 2.71 | 8.31 | 686 | 1 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 1.93 | 3.61 | 3.61 |
| other | 73078 | 0.24 | 0.70 | 8.31 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 804 | 318 | 280 | 1391 | 70073 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.5 | 0 | 336 | 0 | 0 |
| jobs in flight | 0.5 | 0 | 234 | 0 | 0 |
| chunks generating or meshing | 1.9 | 0 | 719 | 0 | 0 |

After a chunk border the world finished the new chunks in 11 ms on average, 16 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 69255 chunks, 8549 meshes (49166 empty), 8549 on the GPU, 1881685 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 68481 | 18.75 | 8.31 | no | no | 49 | 0 | 0 | 0 | 258 | 108 |
| 108 | 0.03 | 4.24 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 7335 | 2.06 | 4.13 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 69609 | 19.06 | 4.12 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 26530 | 7.44 | 4.11 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3556 | 3555 | 4.24 | 804 | 316 | 2 | 336 | 132 |
| 1 | 3550 | 3550 | 3.36 | 804 | 316 | 2 | 330 | 126 |
| 2 | 3519 | 3519 | 4.13 | 804 | 310 | 2 | 274 | 120 |
| 3 | 3564 | 3564 | 3.90 | 804 | 316 | 2 | 274 | 120 |
| 4 | 3550 | 3550 | 3.95 | 804 | 301 | 2 | 298 | 120 |
| 5 | 3612 | 3612 | 3.98 | 804 | 300 | 2 | 274 | 138 |
| 6 | 3555 | 3555 | 3.50 | 804 | 303 | 2 | 258 | 126 |
| 7 | 3684 | 3684 | 4.11 | 804 | 321 | 2 | 293 | 126 |
| 8 | 3736 | 3737 | 3.43 | 804 | 328 | 2 | 281 | 138 |
| 9 | 3743 | 3742 | 3.04 | 804 | 334 | 2 | 324 | 108 |
| 10 | 3719 | 3720 | 3.87 | 804 | 318 | 2 | 293 | 114 |
| 11 | 3722 | 3722 | 3.74 | 804 | 310 | 2 | 271 | 150 |
| 12 | 3698 | 3698 | 3.56 | 804 | 320 | 2 | 324 | 120 |
| 13 | 3724 | 3722 | 3.51 | 804 | 308 | 2 | 299 | 120 |
| 14 | 3651 | 3653 | 3.90 | 804 | 326 | 2 | 263 | 126 |
| 15 | 3637 | 3637 | 3.71 | 804 | 335 | 2 | 249 | 132 |
| 16 | 3685 | 3685 | 3.97 | 804 | 329 | 2 | 271 | 132 |
| 17 | 3732 | 3732 | 2.74 | 804 | 326 | 2 | 306 | 138 |
| 18 | 3748 | 3747 | 8.31 | 804 | 330 | 2 | 281 | 234 |
| 19 | 3733 | 3734 | 4.12 | 804 | 318 | 2 | 305 | 139 |

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
    "vertical": "8",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.709,
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
    "load_start": 2.13,
    "load_end": 2.24,
    "unix_time": 1791214794,
    "date_utc": "2026-10-05 15:39:54 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 73118,
    "seconds": 20,
    "avg_fps": 3655.9,
    "mean_ms": 0.2735,
    "p50_ms": 0.2406,
    "p95_ms": 0.534,
    "p99_ms": 0.7053,
    "p999_ms": 2.7123,
    "min_ms": 0.0724,
    "max_ms": 8.309,
    "low_1_fps": 686,
    "over_144hz": 1,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 1.929,
      "p99_ms": 3.6082,
      "max_ms": 3.6082
    },
    "other": {
      "frames": 73078,
      "p50_ms": 0.2405,
      "p99_ms": 0.6969,
      "max_ms": 8.309
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
    "upload_quads_per_s": 70073.2,
    "crossings": 40,
    "queued": {
      "avg": 0.5,
      "p95": 0,
      "max": 336,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.5,
      "p95": 0,
      "max": 234,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 1.9,
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
      "avg_ms": 10.9204,
      "max_ms": 15.793
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 69255,
      "data_ready": 69255,
      "mesh_ready": 8549,
      "mesh_empty": 49166,
      "gpu_chunks": 8549,
      "quads": 1881685,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 68481,
      "t_s": 18.752,
      "ms": 8.309,
      "crossed": false,
      "presented": false,
      "generated": 49,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 258,
      "in_flight": 108
    },
    {
      "frame": 108,
      "t_s": 0.03,
      "ms": 4.2371,
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
      "frame": 7335,
      "t_s": 2.063,
      "ms": 4.1295,
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
      "frame": 69609,
      "t_s": 19.061,
      "ms": 4.1208,
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
      "frame": 26530,
      "t_s": 7.439,
      "ms": 4.1097,
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
      "frames": 3556,
      "fps": 3555.3,
      "worst_ms": 4.2371,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 336,
      "in_flight_max": 132
    },
    {
      "second": 1,
      "frames": 3550,
      "fps": 3550.5,
      "worst_ms": 3.3553,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 330,
      "in_flight_max": 126
    },
    {
      "second": 2,
      "frames": 3519,
      "fps": 3518.8,
      "worst_ms": 4.1295,
      "generated": 804,
      "meshes": 310,
      "crossings": 2,
      "queued_max": 274,
      "in_flight_max": 120
    },
    {
      "second": 3,
      "frames": 3564,
      "fps": 3564.2,
      "worst_ms": 3.9036,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 274,
      "in_flight_max": 120
    },
    {
      "second": 4,
      "frames": 3550,
      "fps": 3549.9,
      "worst_ms": 3.9488,
      "generated": 804,
      "meshes": 301,
      "crossings": 2,
      "queued_max": 298,
      "in_flight_max": 120
    },
    {
      "second": 5,
      "frames": 3612,
      "fps": 3611.8,
      "worst_ms": 3.9756,
      "generated": 804,
      "meshes": 300,
      "crossings": 2,
      "queued_max": 274,
      "in_flight_max": 138
    },
    {
      "second": 6,
      "frames": 3555,
      "fps": 3554.5,
      "worst_ms": 3.4988,
      "generated": 804,
      "meshes": 303,
      "crossings": 2,
      "queued_max": 258,
      "in_flight_max": 126
    },
    {
      "second": 7,
      "frames": 3684,
      "fps": 3684.2,
      "worst_ms": 4.1097,
      "generated": 804,
      "meshes": 321,
      "crossings": 2,
      "queued_max": 293,
      "in_flight_max": 126
    },
    {
      "second": 8,
      "frames": 3736,
      "fps": 3736.8,
      "worst_ms": 3.4283,
      "generated": 804,
      "meshes": 328,
      "crossings": 2,
      "queued_max": 281,
      "in_flight_max": 138
    },
    {
      "second": 9,
      "frames": 3743,
      "fps": 3742.4,
      "worst_ms": 3.0411,
      "generated": 804,
      "meshes": 334,
      "crossings": 2,
      "queued_max": 324,
      "in_flight_max": 108
    },
    {
      "second": 10,
      "frames": 3719,
      "fps": 3719.6,
      "worst_ms": 3.8691,
      "generated": 804,
      "meshes": 318,
      "crossings": 2,
      "queued_max": 293,
      "in_flight_max": 114
    },
    {
      "second": 11,
      "frames": 3722,
      "fps": 3721.7,
      "worst_ms": 3.7445,
      "generated": 804,
      "meshes": 310,
      "crossings": 2,
      "queued_max": 271,
      "in_flight_max": 150
    },
    {
      "second": 12,
      "frames": 3698,
      "fps": 3697.6,
      "worst_ms": 3.5611,
      "generated": 804,
      "meshes": 320,
      "crossings": 2,
      "queued_max": 324,
      "in_flight_max": 120
    },
    {
      "second": 13,
      "frames": 3724,
      "fps": 3722.4,
      "worst_ms": 3.5133,
      "generated": 804,
      "meshes": 308,
      "crossings": 2,
      "queued_max": 299,
      "in_flight_max": 120
    },
    {
      "second": 14,
      "frames": 3651,
      "fps": 3652.8,
      "worst_ms": 3.9007,
      "generated": 804,
      "meshes": 326,
      "crossings": 2,
      "queued_max": 263,
      "in_flight_max": 126
    },
    {
      "second": 15,
      "frames": 3637,
      "fps": 3637.2,
      "worst_ms": 3.7141,
      "generated": 804,
      "meshes": 335,
      "crossings": 2,
      "queued_max": 249,
      "in_flight_max": 132
    },
    {
      "second": 16,
      "frames": 3685,
      "fps": 3685.2,
      "worst_ms": 3.9702,
      "generated": 804,
      "meshes": 329,
      "crossings": 2,
      "queued_max": 271,
      "in_flight_max": 132
    },
    {
      "second": 17,
      "frames": 3732,
      "fps": 3732,
      "worst_ms": 2.7356,
      "generated": 804,
      "meshes": 326,
      "crossings": 2,
      "queued_max": 306,
      "in_flight_max": 138
    },
    {
      "second": 18,
      "frames": 3748,
      "fps": 3746.8,
      "worst_ms": 8.309,
      "generated": 804,
      "meshes": 330,
      "crossings": 2,
      "queued_max": 281,
      "in_flight_max": 234
    },
    {
      "second": 19,
      "frames": 3733,
      "fps": 3733.6,
      "worst_ms": 4.1208,
      "generated": 804,
      "meshes": 318,
      "crossings": 2,
      "queued_max": 305,
      "in_flight_max": 139
    }
  ]
}
```
