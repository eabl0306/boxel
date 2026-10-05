# boxel stress: fly r32 32 vox/s

- Run: 2026-10-05 15:39:09 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 3.34 -> 2.68.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 0.68 s with 65987 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 73070 | 3653 | 0.24 | 0.53 | 0.70 | 2.79 | 4.40 | 762 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 1.94 | 3.70 | 3.70 |
| other | 73050 | 0.24 | 0.70 | 4.40 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 402 | 157 | 122 | 694 | 35285 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.3 | 0 | 336 | 0 | 0 |
| jobs in flight | 0.2 | 0 | 198 | 0 | 0 |
| chunks generating or meshing | 0.9 | 0 | 719 | 0 | 0 |

After a chunk border the world finished the new chunks in 11 ms on average, 14 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 69255 chunks, 8496 meshes (49179 empty), 8496 on the GPU, 1904612 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 31979 | 8.91 | 4.40 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 33841 | 9.42 | 4.27 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 37099 | 10.33 | 4.22 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 25651 | 7.16 | 4.18 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 1288 | 0.35 | 4.17 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3615 | 3604 | 4.17 | 402 | 159 | 1 | 277 | 90 |
| 1 | 3592 | 3603 | 3.45 | 402 | 157 | 1 | 336 | 108 |
| 2 | 3579 | 3579 | 3.44 | 402 | 158 | 1 | 274 | 108 |
| 3 | 3577 | 3577 | 3.38 | 402 | 158 | 1 | 292 | 102 |
| 4 | 3574 | 3574 | 3.61 | 402 | 156 | 1 | 294 | 138 |
| 5 | 3574 | 3573 | 3.23 | 402 | 154 | 1 | 266 | 109 |
| 6 | 3566 | 3567 | 3.70 | 402 | 157 | 1 | 226 | 156 |
| 7 | 3598 | 3597 | 4.18 | 402 | 159 | 1 | 275 | 132 |
| 8 | 3617 | 3618 | 4.40 | 402 | 150 | 1 | 252 | 126 |
| 9 | 3593 | 3592 | 4.27 | 402 | 151 | 1 | 251 | 132 |
| 10 | 3602 | 3601 | 4.22 | 402 | 149 | 1 | 268 | 120 |
| 11 | 3666 | 3665 | 2.92 | 402 | 151 | 1 | 266 | 126 |
| 12 | 3702 | 3704 | 3.31 | 402 | 152 | 1 | 256 | 132 |
| 13 | 3710 | 3710 | 3.92 | 402 | 151 | 1 | 304 | 114 |
| 14 | 3725 | 3725 | 3.91 | 402 | 158 | 1 | 271 | 132 |
| 15 | 3754 | 3753 | 3.66 | 402 | 163 | 1 | 257 | 110 |
| 16 | 3766 | 3767 | 3.33 | 402 | 166 | 1 | 237 | 138 |
| 17 | 3754 | 3754 | 3.24 | 402 | 162 | 1 | 129 | 198 |
| 18 | 3763 | 3763 | 3.52 | 402 | 167 | 1 | 323 | 114 |
| 19 | 3743 | 3743 | 3.26 | 402 | 167 | 1 | 249 | 138 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "after-r1",
    "radius": 32,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "vertical": "8",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.68,
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
    "load_start": 3.34,
    "load_end": 2.68,
    "unix_time": 1791214749,
    "date_utc": "2026-10-05 15:39:09 UTC",
    "frames_presented": 2867
  },
  "frames": {
    "count": 73070,
    "seconds": 20,
    "avg_fps": 3653.5,
    "mean_ms": 0.2737,
    "p50_ms": 0.2434,
    "p95_ms": 0.5325,
    "p99_ms": 0.6977,
    "p999_ms": 2.7901,
    "min_ms": 0.0758,
    "max_ms": 4.3974,
    "low_1_fps": 762.4,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 1.9386,
      "p99_ms": 3.6972,
      "max_ms": 3.6972
    },
    "other": {
      "frames": 73050,
      "p50_ms": 0.2434,
      "p99_ms": 0.6953,
      "max_ms": 4.3974
    }
  },
  "world": {
    "generated": 8040,
    "generated_per_s": 402,
    "meshes": 3145,
    "meshes_per_s": 157.2,
    "removed": 2443,
    "removed_per_s": 122.1,
    "jobs": 13883,
    "jobs_per_s": 694.1,
    "upload_quads": 705712,
    "upload_quads_per_s": 35285.5,
    "crossings": 20,
    "queued": {
      "avg": 0.3,
      "p95": 0,
      "max": 336,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.2,
      "p95": 0,
      "max": 198,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 0.9,
      "p95": 0,
      "max": 719,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "cut_off": 0,
      "avg_ms": 10.6978,
      "max_ms": 13.6791
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 69255,
      "data_ready": 69255,
      "mesh_ready": 8496,
      "mesh_empty": 49179,
      "gpu_chunks": 8496,
      "quads": 1904612,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 31979,
      "t_s": 8.91,
      "ms": 4.3974,
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
      "frame": 33841,
      "t_s": 9.423,
      "ms": 4.274,
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
      "frame": 37099,
      "t_s": 10.331,
      "ms": 4.2249,
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
      "frame": 25651,
      "t_s": 7.156,
      "ms": 4.1799,
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
      "frame": 1288,
      "t_s": 0.351,
      "ms": 4.1732,
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
      "frames": 3615,
      "fps": 3603.5,
      "worst_ms": 4.1732,
      "generated": 402,
      "meshes": 159,
      "crossings": 1,
      "queued_max": 277,
      "in_flight_max": 90
    },
    {
      "second": 1,
      "frames": 3592,
      "fps": 3603.2,
      "worst_ms": 3.4524,
      "generated": 402,
      "meshes": 157,
      "crossings": 1,
      "queued_max": 336,
      "in_flight_max": 108
    },
    {
      "second": 2,
      "frames": 3579,
      "fps": 3579.2,
      "worst_ms": 3.4414,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 274,
      "in_flight_max": 108
    },
    {
      "second": 3,
      "frames": 3577,
      "fps": 3576.7,
      "worst_ms": 3.3809,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 292,
      "in_flight_max": 102
    },
    {
      "second": 4,
      "frames": 3574,
      "fps": 3574.2,
      "worst_ms": 3.6057,
      "generated": 402,
      "meshes": 156,
      "crossings": 1,
      "queued_max": 294,
      "in_flight_max": 138
    },
    {
      "second": 5,
      "frames": 3574,
      "fps": 3573.1,
      "worst_ms": 3.2322,
      "generated": 402,
      "meshes": 154,
      "crossings": 1,
      "queued_max": 266,
      "in_flight_max": 109
    },
    {
      "second": 6,
      "frames": 3566,
      "fps": 3566.9,
      "worst_ms": 3.6972,
      "generated": 402,
      "meshes": 157,
      "crossings": 1,
      "queued_max": 226,
      "in_flight_max": 156
    },
    {
      "second": 7,
      "frames": 3598,
      "fps": 3597.4,
      "worst_ms": 4.1799,
      "generated": 402,
      "meshes": 159,
      "crossings": 1,
      "queued_max": 275,
      "in_flight_max": 132
    },
    {
      "second": 8,
      "frames": 3617,
      "fps": 3617.8,
      "worst_ms": 4.3974,
      "generated": 402,
      "meshes": 150,
      "crossings": 1,
      "queued_max": 252,
      "in_flight_max": 126
    },
    {
      "second": 9,
      "frames": 3593,
      "fps": 3592.4,
      "worst_ms": 4.274,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 251,
      "in_flight_max": 132
    },
    {
      "second": 10,
      "frames": 3602,
      "fps": 3601,
      "worst_ms": 4.2249,
      "generated": 402,
      "meshes": 149,
      "crossings": 1,
      "queued_max": 268,
      "in_flight_max": 120
    },
    {
      "second": 11,
      "frames": 3666,
      "fps": 3665,
      "worst_ms": 2.924,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 266,
      "in_flight_max": 126
    },
    {
      "second": 12,
      "frames": 3702,
      "fps": 3704.3,
      "worst_ms": 3.3094,
      "generated": 402,
      "meshes": 152,
      "crossings": 1,
      "queued_max": 256,
      "in_flight_max": 132
    },
    {
      "second": 13,
      "frames": 3710,
      "fps": 3709.9,
      "worst_ms": 3.9194,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 304,
      "in_flight_max": 114
    },
    {
      "second": 14,
      "frames": 3725,
      "fps": 3725.2,
      "worst_ms": 3.9083,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 271,
      "in_flight_max": 132
    },
    {
      "second": 15,
      "frames": 3754,
      "fps": 3753,
      "worst_ms": 3.6607,
      "generated": 402,
      "meshes": 163,
      "crossings": 1,
      "queued_max": 257,
      "in_flight_max": 110
    },
    {
      "second": 16,
      "frames": 3766,
      "fps": 3767.1,
      "worst_ms": 3.3309,
      "generated": 402,
      "meshes": 166,
      "crossings": 1,
      "queued_max": 237,
      "in_flight_max": 138
    },
    {
      "second": 17,
      "frames": 3754,
      "fps": 3753.7,
      "worst_ms": 3.242,
      "generated": 402,
      "meshes": 162,
      "crossings": 1,
      "queued_max": 129,
      "in_flight_max": 198
    },
    {
      "second": 18,
      "frames": 3763,
      "fps": 3763.3,
      "worst_ms": 3.524,
      "generated": 402,
      "meshes": 167,
      "crossings": 1,
      "queued_max": 323,
      "in_flight_max": 114
    },
    {
      "second": 19,
      "frames": 3743,
      "fps": 3743.1,
      "worst_ms": 3.2559,
      "generated": 402,
      "meshes": 167,
      "crossings": 1,
      "queued_max": 249,
      "in_flight_max": 138
    }
  ]
}
```
