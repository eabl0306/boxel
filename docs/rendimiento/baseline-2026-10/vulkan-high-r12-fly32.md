# boxel stress: fly r12 32 vox/s

- Run: 2026-10-05 13:58:58 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 2.87 -> 3.86.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 12, settled in 0.73 s with 3270 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 129442 | 6472 | 0.13 | 0.31 | 0.46 | 2.59 | 7.25 | 906 | 1 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 1.32 | 2.63 | 2.63 |
| other | 129422 | 0.13 | 0.46 | 7.25 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 162 | 61 | 49 | 276 | 13682 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.5 | 0 | 152 | 0 | 0 |
| jobs in flight | 0.0 | 0 | 12 | 0 | 0 |
| chunks generating or meshing | 1.1 | 0 | 286 | 0 | 0 |

After a chunk border the world finished the new chunks in 13 ms on average, 30 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 3690 chunks, 1330 meshes (1819 empty), 1330 on the GPU, 298478 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 107683 | 17.20 | 7.25 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 107878 | 17.26 | 4.88 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 14393 | 2.08 | 4.40 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 14470 | 2.12 | 4.11 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 107679 | 17.18 | 3.93 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 7265 | 7264 | 2.78 | 162 | 68 | 1 | 150 | 12 |
| 1 | 6803 | 6803 | 3.43 | 162 | 64 | 1 | 151 | 12 |
| 2 | 5235 | 5235 | 4.40 | 162 | 58 | 1 | 151 | 12 |
| 3 | 6189 | 6189 | 2.86 | 162 | 59 | 1 | 152 | 12 |
| 4 | 6506 | 6506 | 3.13 | 162 | 59 | 1 | 151 | 12 |
| 5 | 6003 | 6003 | 2.99 | 162 | 60 | 1 | 152 | 12 |
| 6 | 5874 | 5872 | 3.42 | 162 | 61 | 1 | 152 | 12 |
| 7 | 5984 | 5986 | 2.96 | 162 | 59 | 1 | 152 | 12 |
| 8 | 6415 | 6414 | 3.17 | 162 | 61 | 1 | 152 | 12 |
| 9 | 6711 | 6711 | 2.87 | 162 | 61 | 1 | 151 | 12 |
| 10 | 6082 | 6082 | 3.49 | 162 | 62 | 1 | 152 | 12 |
| 11 | 6208 | 6207 | 2.90 | 162 | 65 | 1 | 152 | 12 |
| 12 | 6247 | 6248 | 3.42 | 162 | 65 | 1 | 152 | 12 |
| 13 | 6418 | 6418 | 3.21 | 162 | 62 | 1 | 152 | 12 |
| 14 | 6197 | 6197 | 3.02 | 162 | 61 | 1 | 152 | 12 |
| 15 | 6247 | 6247 | 2.91 | 162 | 62 | 1 | 151 | 12 |
| 16 | 6202 | 6201 | 2.93 | 162 | 59 | 1 | 152 | 12 |
| 17 | 5479 | 5479 | 7.25 | 162 | 59 | 1 | 152 | 12 |
| 18 | 8182 | 8182 | 3.71 | 162 | 56 | 1 | 152 | 12 |
| 19 | 9195 | 9195 | 3.81 | 162 | 58 | 1 | 152 | 12 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "e933486",
    "radius": 12,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.727,
    "settle_chunks": 3270,
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
    "load_start": 2.87,
    "load_end": 3.86,
    "unix_time": 1791208738,
    "date_utc": "2026-10-05 13:58:58 UTC",
    "frames_presented": 2869
  },
  "frames": {
    "count": 129442,
    "seconds": 20,
    "avg_fps": 6472.1,
    "mean_ms": 0.1545,
    "p50_ms": 0.1277,
    "p95_ms": 0.3087,
    "p99_ms": 0.4604,
    "p999_ms": 2.5939,
    "min_ms": 0.0669,
    "max_ms": 7.2545,
    "low_1_fps": 906.3,
    "over_144hz": 1,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 1.3203,
      "p99_ms": 2.6318,
      "max_ms": 2.6318
    },
    "other": {
      "frames": 129422,
      "p50_ms": 0.1277,
      "p99_ms": 0.4591,
      "max_ms": 7.2545
    }
  },
  "world": {
    "generated": 3240,
    "generated_per_s": 162,
    "meshes": 1219,
    "meshes_per_s": 60.9,
    "removed": 985,
    "removed_per_s": 49.2,
    "jobs": 5515,
    "jobs_per_s": 275.7,
    "upload_quads": 273649,
    "upload_quads_per_s": 13682.4,
    "crossings": 20,
    "queued": {
      "avg": 0.5,
      "p95": 0,
      "max": 152,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0,
      "p95": 0,
      "max": 12,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 1.1,
      "p95": 0,
      "max": 286,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "avg_ms": 13.4986,
      "max_ms": 30.1533
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 3690,
      "data_ready": 3690,
      "mesh_ready": 1330,
      "mesh_empty": 1819,
      "gpu_chunks": 1330,
      "quads": 298478,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 107683,
      "t_s": 17.201,
      "ms": 7.2545,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 107878,
      "t_s": 17.264,
      "ms": 4.8845,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 14393,
      "t_s": 2.078,
      "ms": 4.4004,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 14470,
      "t_s": 2.12,
      "ms": 4.1142,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 107679,
      "t_s": 17.185,
      "ms": 3.9324,
      "crossed": false,
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
      "frames": 7265,
      "fps": 7264.3,
      "worst_ms": 2.7786,
      "generated": 162,
      "meshes": 68,
      "crossings": 1,
      "queued_max": 150,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 6803,
      "fps": 6802.9,
      "worst_ms": 3.4267,
      "generated": 162,
      "meshes": 64,
      "crossings": 1,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 5235,
      "fps": 5235.4,
      "worst_ms": 4.4004,
      "generated": 162,
      "meshes": 58,
      "crossings": 1,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 6189,
      "fps": 6188.6,
      "worst_ms": 2.8603,
      "generated": 162,
      "meshes": 59,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 6506,
      "fps": 6506.2,
      "worst_ms": 3.131,
      "generated": 162,
      "meshes": 59,
      "crossings": 1,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 6003,
      "fps": 6003.2,
      "worst_ms": 2.9897,
      "generated": 162,
      "meshes": 60,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 5874,
      "fps": 5872.3,
      "worst_ms": 3.4189,
      "generated": 162,
      "meshes": 61,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 5984,
      "fps": 5985.9,
      "worst_ms": 2.9561,
      "generated": 162,
      "meshes": 59,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 6415,
      "fps": 6414.4,
      "worst_ms": 3.1719,
      "generated": 162,
      "meshes": 61,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 6711,
      "fps": 6711,
      "worst_ms": 2.8669,
      "generated": 162,
      "meshes": 61,
      "crossings": 1,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 6082,
      "fps": 6082.2,
      "worst_ms": 3.4899,
      "generated": 162,
      "meshes": 62,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 6208,
      "fps": 6206.8,
      "worst_ms": 2.9025,
      "generated": 162,
      "meshes": 65,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 6247,
      "fps": 6248.2,
      "worst_ms": 3.4214,
      "generated": 162,
      "meshes": 65,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 6418,
      "fps": 6417.8,
      "worst_ms": 3.2117,
      "generated": 162,
      "meshes": 62,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 6197,
      "fps": 6197.2,
      "worst_ms": 3.0204,
      "generated": 162,
      "meshes": 61,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 6247,
      "fps": 6247,
      "worst_ms": 2.91,
      "generated": 162,
      "meshes": 62,
      "crossings": 1,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 6202,
      "fps": 6201.4,
      "worst_ms": 2.9295,
      "generated": 162,
      "meshes": 59,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 5479,
      "fps": 5479.3,
      "worst_ms": 7.2545,
      "generated": 162,
      "meshes": 59,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 8182,
      "fps": 8181.9,
      "worst_ms": 3.7128,
      "generated": 162,
      "meshes": 56,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 9195,
      "fps": 9195.1,
      "worst_ms": 3.8058,
      "generated": 162,
      "meshes": 58,
      "crossings": 1,
      "queued_max": 152,
      "in_flight_max": 12
    }
  ]
}
```
