# boxel stress: fly r32 32 vox/s

- Run: 2026-10-05 14:02:29 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 3.88 -> 3.30.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 1.69 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 66152 | 3308 | 0.25 | 0.68 | 0.96 | 2.93 | 8.43 | 596 | 5 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 6.14 | 8.43 | 8.43 |
| other | 66132 | 0.25 | 0.95 | 4.80 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 402 | 157 | 122 | 694 | 35285 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 6.3 | 0 | 392 | 0 | 0 |
| jobs in flight | 0.2 | 0 | 12 | 0 | 0 |
| chunks generating or meshing | 12.5 | 0 | 719 | 0 | 0 |

After a chunk border the world finished the new chunks in 44 ms on average, 48 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 21870 chunks, 8496 meshes (12021 empty), 8496 on the GPU, 1904612 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 43871 | 13.50 | 8.43 | yes | 12 | 0 | 154 | 0 | 390 | 12 |
| 37364 | 11.50 | 8.36 | yes | 12 | 0 | 152 | 0 | 390 | 12 |
| 34145 | 10.50 | 8.02 | yes | 12 | 0 | 151 | 0 | 390 | 12 |
| 40619 | 12.50 | 7.21 | yes | 12 | 0 | 151 | 0 | 390 | 12 |
| 60975 | 18.50 | 7.02 | yes | 12 | 0 | 154 | 0 | 390 | 12 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3301 | 3300 | 4.41 | 402 | 159 | 1 | 392 | 12 |
| 1 | 3252 | 3253 | 6.22 | 402 | 157 | 1 | 392 | 12 |
| 2 | 3257 | 3255 | 5.34 | 402 | 158 | 1 | 392 | 12 |
| 3 | 3263 | 3265 | 4.45 | 402 | 158 | 1 | 392 | 12 |
| 4 | 3262 | 3263 | 5.31 | 402 | 156 | 1 | 391 | 12 |
| 5 | 3302 | 3301 | 6.14 | 402 | 154 | 1 | 390 | 12 |
| 6 | 3222 | 3222 | 5.67 | 402 | 157 | 1 | 390 | 12 |
| 7 | 3251 | 3250 | 5.99 | 402 | 159 | 1 | 390 | 12 |
| 8 | 3194 | 3195 | 6.38 | 402 | 150 | 1 | 390 | 12 |
| 9 | 3213 | 3213 | 6.17 | 402 | 151 | 1 | 390 | 12 |
| 10 | 3201 | 3201 | 8.02 | 402 | 149 | 1 | 390 | 12 |
| 11 | 3250 | 3250 | 8.36 | 402 | 151 | 1 | 390 | 12 |
| 12 | 3258 | 3258 | 7.21 | 402 | 152 | 1 | 390 | 12 |
| 13 | 3281 | 3281 | 8.43 | 402 | 151 | 1 | 391 | 12 |
| 14 | 3396 | 3397 | 6.41 | 402 | 158 | 1 | 392 | 12 |
| 15 | 3386 | 3386 | 6.31 | 402 | 163 | 1 | 392 | 12 |
| 16 | 3487 | 3487 | 5.52 | 402 | 166 | 1 | 392 | 12 |
| 17 | 3471 | 3471 | 5.11 | 402 | 162 | 1 | 392 | 12 |
| 18 | 3453 | 3452 | 7.02 | 402 | 167 | 1 | 392 | 12 |
| 19 | 3452 | 3452 | 5.22 | 402 | 167 | 1 | 391 | 12 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "e933486",
    "radius": 32,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 1.694,
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
    "load_start": 3.88,
    "load_end": 3.3,
    "unix_time": 1791208949,
    "date_utc": "2026-10-05 14:02:29 UTC",
    "frames_presented": 2856
  },
  "frames": {
    "count": 66152,
    "seconds": 20,
    "avg_fps": 3307.6,
    "mean_ms": 0.3023,
    "p50_ms": 0.2476,
    "p95_ms": 0.678,
    "p99_ms": 0.9552,
    "p999_ms": 2.9348,
    "min_ms": 0.0972,
    "max_ms": 8.4294,
    "low_1_fps": 596.5,
    "over_144hz": 5,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 6.1371,
      "p99_ms": 8.4294,
      "max_ms": 8.4294
    },
    "other": {
      "frames": 66132,
      "p50_ms": 0.2476,
      "p99_ms": 0.9479,
      "max_ms": 4.7999
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
    "upload_quads_per_s": 35285.4,
    "crossings": 20,
    "queued": {
      "avg": 6.3,
      "p95": 0,
      "max": 392,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.2,
      "p95": 0,
      "max": 12,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 12.5,
      "p95": 0,
      "max": 719,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "avg_ms": 43.6555,
      "max_ms": 47.7241
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 21870,
      "data_ready": 21870,
      "mesh_ready": 8496,
      "mesh_empty": 12021,
      "gpu_chunks": 8496,
      "quads": 1904612,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 43871,
      "t_s": 13.5,
      "ms": 8.4294,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 154,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 37364,
      "t_s": 11.5,
      "ms": 8.3622,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 152,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 34145,
      "t_s": 10.501,
      "ms": 8.0208,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 151,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 40619,
      "t_s": 12.5,
      "ms": 7.2109,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 151,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 60975,
      "t_s": 18.5,
      "ms": 7.0158,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 154,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    }
  ],
  "timeline": [
    {
      "second": 0,
      "frames": 3301,
      "fps": 3300.1,
      "worst_ms": 4.4106,
      "generated": 402,
      "meshes": 159,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 3252,
      "fps": 3252.8,
      "worst_ms": 6.2206,
      "generated": 402,
      "meshes": 157,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 3257,
      "fps": 3254.7,
      "worst_ms": 5.3414,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 3263,
      "fps": 3264.8,
      "worst_ms": 4.45,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 3262,
      "fps": 3262.6,
      "worst_ms": 5.3052,
      "generated": 402,
      "meshes": 156,
      "crossings": 1,
      "queued_max": 391,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 3302,
      "fps": 3301.4,
      "worst_ms": 6.1371,
      "generated": 402,
      "meshes": 154,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 3222,
      "fps": 3222.1,
      "worst_ms": 5.6666,
      "generated": 402,
      "meshes": 157,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 3251,
      "fps": 3250.1,
      "worst_ms": 5.9892,
      "generated": 402,
      "meshes": 159,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 3194,
      "fps": 3195.1,
      "worst_ms": 6.3761,
      "generated": 402,
      "meshes": 150,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 3213,
      "fps": 3212.6,
      "worst_ms": 6.1673,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 3201,
      "fps": 3201,
      "worst_ms": 8.0208,
      "generated": 402,
      "meshes": 149,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 3250,
      "fps": 3250.1,
      "worst_ms": 8.3622,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 3258,
      "fps": 3257.9,
      "worst_ms": 7.2109,
      "generated": 402,
      "meshes": 152,
      "crossings": 1,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 3281,
      "fps": 3281,
      "worst_ms": 8.4294,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 391,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 3396,
      "fps": 3396.7,
      "worst_ms": 6.4111,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 3386,
      "fps": 3385.7,
      "worst_ms": 6.3051,
      "generated": 402,
      "meshes": 163,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 3487,
      "fps": 3486.9,
      "worst_ms": 5.525,
      "generated": 402,
      "meshes": 166,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 3471,
      "fps": 3471.1,
      "worst_ms": 5.106,
      "generated": 402,
      "meshes": 162,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 3453,
      "fps": 3452.5,
      "worst_ms": 7.0158,
      "generated": 402,
      "meshes": 167,
      "crossings": 1,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 3452,
      "fps": 3452.3,
      "worst_ms": 5.2211,
      "generated": 402,
      "meshes": 167,
      "crossings": 1,
      "queued_max": 391,
      "in_flight_max": 12
    }
  ]
}
```
