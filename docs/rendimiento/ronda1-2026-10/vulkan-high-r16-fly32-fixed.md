# boxel stress: fly r16 32 vox/s

- Run: 2026-10-05 15:37:15 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 12.03 -> 9.36.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.43 s with 5598 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 159603 | 7980 | 0.10 | 0.22 | 0.53 | 2.19 | 4.09 | 985 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 0.69 | 1.93 | 1.93 |
| other | 159583 | 0.10 | 0.53 | 4.09 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 210 | 79 | 63 | 360 | 18092 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.2 | 0 | 186 | 0 | 0 |
| jobs in flight | 0.1 | 0 | 96 | 0 | 0 |
| chunks generating or meshing | 0.5 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 5 ms on average, 6 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 6102 chunks, 2270 meshes (3129 empty), 2270 on the GPU, 508896 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 13493 | 1.75 | 4.09 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 79153 | 9.84 | 4.03 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 39439 | 5.01 | 3.99 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 28182 | 3.59 | 3.98 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 82585 | 10.26 | 3.97 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 7650 | 7650 | 3.78 | 210 | 79 | 1 | 186 | 42 |
| 1 | 7803 | 7802 | 4.09 | 210 | 80 | 1 | 186 | 36 |
| 2 | 8026 | 8027 | 3.21 | 210 | 83 | 1 | 174 | 42 |
| 3 | 7947 | 7946 | 3.98 | 210 | 76 | 1 | 186 | 54 |
| 4 | 7964 | 7964 | 3.70 | 210 | 81 | 1 | 180 | 36 |
| 5 | 7961 | 7961 | 3.99 | 210 | 84 | 1 | 174 | 42 |
| 6 | 8187 | 8187 | 3.63 | 210 | 83 | 1 | 186 | 42 |
| 7 | 8226 | 8226 | 3.63 | 210 | 83 | 1 | 174 | 36 |
| 8 | 8366 | 8366 | 3.51 | 210 | 83 | 1 | 180 | 42 |
| 9 | 8290 | 8289 | 4.03 | 210 | 84 | 1 | 186 | 32 |
| 10 | 8109 | 8110 | 3.97 | 210 | 84 | 1 | 174 | 54 |
| 11 | 8274 | 8274 | 3.57 | 210 | 79 | 1 | 186 | 43 |
| 12 | 7925 | 7924 | 3.27 | 210 | 80 | 1 | 180 | 36 |
| 13 | 7734 | 7723 | 3.18 | 210 | 78 | 1 | 186 | 48 |
| 14 | 7791 | 7804 | 3.93 | 210 | 73 | 1 | 119 | 96 |
| 15 | 7820 | 7820 | 3.82 | 210 | 71 | 1 | 126 | 90 |
| 16 | 7968 | 7968 | 3.63 | 210 | 75 | 1 | 180 | 42 |
| 17 | 7727 | 7727 | 3.16 | 210 | 72 | 1 | 180 | 36 |
| 18 | 7944 | 7944 | 3.21 | 210 | 77 | 1 | 180 | 54 |
| 19 | 7891 | 7891 | 3.69 | 210 | 79 | 1 | 180 | 36 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "after-r1",
    "radius": 16,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "vertical": "fixed",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.432,
    "settle_chunks": 5598,
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
    "load_start": 12.03,
    "load_end": 9.36,
    "unix_time": 1791214635,
    "date_utc": "2026-10-05 15:37:15 UTC",
    "frames_presented": 2861
  },
  "frames": {
    "count": 159603,
    "seconds": 20,
    "avg_fps": 7980.1,
    "mean_ms": 0.1253,
    "p50_ms": 0.103,
    "p95_ms": 0.2165,
    "p99_ms": 0.5298,
    "p999_ms": 2.1862,
    "min_ms": 0.0639,
    "max_ms": 4.0895,
    "low_1_fps": 985,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 0.6895,
      "p99_ms": 1.9285,
      "max_ms": 1.9285
    },
    "other": {
      "frames": 159583,
      "p50_ms": 0.103,
      "p99_ms": 0.5283,
      "max_ms": 4.0895
    }
  },
  "world": {
    "generated": 4200,
    "generated_per_s": 210,
    "meshes": 1584,
    "meshes_per_s": 79.2,
    "removed": 1265,
    "removed_per_s": 63.2,
    "jobs": 7203,
    "jobs_per_s": 360.1,
    "upload_quads": 361844,
    "upload_quads_per_s": 18092.1,
    "crossings": 20,
    "queued": {
      "avg": 0.2,
      "p95": 0,
      "max": 186,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.1,
      "p95": 0,
      "max": 96,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 0.5,
      "p95": 0,
      "max": 374,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "cut_off": 0,
      "avg_ms": 4.7833,
      "max_ms": 5.6779
    },
    "catch_up_s": 0,
    "final": {
      "chunks": 6102,
      "data_ready": 6102,
      "mesh_ready": 2270,
      "mesh_empty": 3129,
      "gpu_chunks": 2270,
      "quads": 508896,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 13493,
      "t_s": 1.746,
      "ms": 4.0895,
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
      "frame": 79153,
      "t_s": 9.845,
      "ms": 4.0289,
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
      "frame": 39439,
      "t_s": 5.006,
      "ms": 3.9871,
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
      "frame": 28182,
      "t_s": 3.585,
      "ms": 3.9752,
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
      "frame": 82585,
      "t_s": 10.262,
      "ms": 3.9654,
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
      "frames": 7650,
      "fps": 7649.8,
      "worst_ms": 3.7788,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 42
    },
    {
      "second": 1,
      "frames": 7803,
      "fps": 7802.5,
      "worst_ms": 4.0895,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 36
    },
    {
      "second": 2,
      "frames": 8026,
      "fps": 8026.6,
      "worst_ms": 3.211,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 174,
      "in_flight_max": 42
    },
    {
      "second": 3,
      "frames": 7947,
      "fps": 7946.3,
      "worst_ms": 3.9752,
      "generated": 210,
      "meshes": 76,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 54
    },
    {
      "second": 4,
      "frames": 7964,
      "fps": 7964.2,
      "worst_ms": 3.7039,
      "generated": 210,
      "meshes": 81,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 36
    },
    {
      "second": 5,
      "frames": 7961,
      "fps": 7960.5,
      "worst_ms": 3.9871,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 174,
      "in_flight_max": 42
    },
    {
      "second": 6,
      "frames": 8187,
      "fps": 8187.3,
      "worst_ms": 3.6297,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 42
    },
    {
      "second": 7,
      "frames": 8226,
      "fps": 8226.3,
      "worst_ms": 3.6346,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 174,
      "in_flight_max": 36
    },
    {
      "second": 8,
      "frames": 8366,
      "fps": 8366.4,
      "worst_ms": 3.5107,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 42
    },
    {
      "second": 9,
      "frames": 8290,
      "fps": 8289,
      "worst_ms": 4.0289,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 32
    },
    {
      "second": 10,
      "frames": 8109,
      "fps": 8110.1,
      "worst_ms": 3.9654,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 174,
      "in_flight_max": 54
    },
    {
      "second": 11,
      "frames": 8274,
      "fps": 8273.6,
      "worst_ms": 3.5695,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 43
    },
    {
      "second": 12,
      "frames": 7925,
      "fps": 7923.8,
      "worst_ms": 3.2719,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 36
    },
    {
      "second": 13,
      "frames": 7734,
      "fps": 7722.8,
      "worst_ms": 3.1792,
      "generated": 210,
      "meshes": 78,
      "crossings": 1,
      "queued_max": 186,
      "in_flight_max": 48
    },
    {
      "second": 14,
      "frames": 7791,
      "fps": 7803.6,
      "worst_ms": 3.9262,
      "generated": 210,
      "meshes": 73,
      "crossings": 1,
      "queued_max": 119,
      "in_flight_max": 96
    },
    {
      "second": 15,
      "frames": 7820,
      "fps": 7819.8,
      "worst_ms": 3.8166,
      "generated": 210,
      "meshes": 71,
      "crossings": 1,
      "queued_max": 126,
      "in_flight_max": 90
    },
    {
      "second": 16,
      "frames": 7968,
      "fps": 7968.2,
      "worst_ms": 3.6281,
      "generated": 210,
      "meshes": 75,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 42
    },
    {
      "second": 17,
      "frames": 7727,
      "fps": 7727.1,
      "worst_ms": 3.1615,
      "generated": 210,
      "meshes": 72,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 36
    },
    {
      "second": 18,
      "frames": 7944,
      "fps": 7943.6,
      "worst_ms": 3.2055,
      "generated": 210,
      "meshes": 77,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 54
    },
    {
      "second": 19,
      "frames": 7891,
      "fps": 7890.9,
      "worst_ms": 3.6864,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 180,
      "in_flight_max": 36
    }
  ]
}
```
