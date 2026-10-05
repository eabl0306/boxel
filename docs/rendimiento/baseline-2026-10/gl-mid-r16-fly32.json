# boxel stress: fly r16 32 vox/s

- Run: 2026-10-05 14:03:48 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 1.84 -> 2.27.
- GPU: AMD Radeon 780M Graphics (radeonsi, phoenix, ACO, DRM 3.64, 7.0.0-34-generic), Gl Mid (Other), driver `4.6 (Core Profile) Mesa 26.0.8-1ubuntu0.3`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.74 s with 5598 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 38642 | 1932 | 0.43 | 0.93 | 1.24 | 3.55 | 13.64 | 472 | 15 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 2.91 | 10.82 | 10.82 |
| other | 38622 | 0.43 | 1.23 | 13.64 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 210 | 79 | 63 | 360 | 18092 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 2.7 | 0 | 200 | 0 | 0 |
| jobs in flight | 0.2 | 0 | 12 | 0 | 0 |
| chunks generating or meshing | 5.8 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 51 ms on average, 115 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 6102 chunks, 2270 meshes (3129 empty), 2270 on the GPU, 508896 quads, 1279 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 3231 | 2.55 | 13.64 | no | 10 | 0 | 0 | 0 | 79 | 12 |
| 3230 | 2.54 | 12.16 | no | 0 | 5 | 0 | 2677 | 91 | 0 |
| 3286 | 2.64 | 11.65 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 1846 | 1.50 | 10.82 | yes | 12 | 0 | 0 | 0 | 198 | 12 |
| 3197 | 2.48 | 10.75 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 1321 | 1320 | 3.15 | 210 | 79 | 1 | 200 | 12 |
| 1 | 1103 | 1104 | 10.82 | 210 | 80 | 1 | 200 | 12 |
| 2 | 1280 | 1280 | 13.64 | 210 | 83 | 1 | 200 | 12 |
| 3 | 1528 | 1529 | 10.26 | 210 | 76 | 1 | 200 | 12 |
| 4 | 2358 | 2358 | 2.46 | 210 | 81 | 1 | 200 | 12 |
| 5 | 2334 | 2334 | 2.87 | 210 | 84 | 1 | 200 | 12 |
| 6 | 2355 | 2355 | 3.42 | 210 | 83 | 1 | 200 | 12 |
| 7 | 2370 | 2370 | 2.44 | 210 | 83 | 1 | 200 | 12 |
| 8 | 2423 | 2423 | 3.46 | 210 | 83 | 1 | 200 | 12 |
| 9 | 2406 | 2406 | 2.68 | 210 | 84 | 1 | 200 | 12 |
| 10 | 2367 | 2367 | 2.52 | 210 | 84 | 1 | 200 | 12 |
| 11 | 2036 | 2036 | 2.32 | 210 | 79 | 1 | 200 | 12 |
| 12 | 1353 | 1353 | 4.11 | 210 | 80 | 1 | 200 | 12 |
| 13 | 1285 | 1285 | 3.55 | 210 | 78 | 1 | 200 | 12 |
| 14 | 2230 | 2230 | 2.77 | 210 | 73 | 1 | 200 | 12 |
| 15 | 2179 | 2179 | 2.73 | 210 | 71 | 1 | 200 | 12 |
| 16 | 2238 | 2238 | 3.01 | 210 | 75 | 1 | 200 | 12 |
| 17 | 2013 | 2013 | 2.91 | 210 | 72 | 1 | 200 | 12 |
| 18 | 2072 | 2073 | 3.60 | 210 | 77 | 1 | 200 | 12 |
| 19 | 1391 | 1391 | 3.65 | 210 | 79 | 1 | 200 | 12 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "e933486",
    "radius": 16,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.742,
    "settle_chunks": 5598,
    "adapter": "AMD Radeon 780M Graphics (radeonsi, phoenix, ACO, DRM 3.64, 7.0.0-34-generic)",
    "backend": "Gl",
    "tier": "Mid",
    "class": "Other",
    "driver": "4.6 (Core Profile) Mesa 26.0.8-1ubuntu0.3",
    "present": "immediate",
    "window": "1280x720",
    "build": "release",
    "cpu": "AMD Ryzen 7 250 w/ Radeon 780M Graphics",
    "threads": 16,
    "load_start": 1.84,
    "load_end": 2.27,
    "unix_time": 1791209028,
    "date_utc": "2026-10-05 14:03:48 UTC",
    "frames_presented": 2857
  },
  "frames": {
    "count": 38642,
    "seconds": 20,
    "avg_fps": 1932.1,
    "mean_ms": 0.5176,
    "p50_ms": 0.4346,
    "p95_ms": 0.9324,
    "p99_ms": 1.2439,
    "p999_ms": 3.5527,
    "min_ms": 0.2612,
    "max_ms": 13.6397,
    "low_1_fps": 471.8,
    "over_144hz": 15,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 2.9058,
      "p99_ms": 10.8209,
      "max_ms": 10.8209
    },
    "other": {
      "frames": 38622,
      "p50_ms": 0.4345,
      "p99_ms": 1.2338,
      "max_ms": 13.6397
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
    "upload_quads_per_s": 18092.2,
    "crossings": 20,
    "queued": {
      "avg": 2.7,
      "p95": 0,
      "max": 200,
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
      "avg": 5.8,
      "p95": 0,
      "max": 374,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "avg_ms": 50.856,
      "max_ms": 114.8014
    },
    "catch_up_s": 0.003,
    "final": {
      "chunks": 6102,
      "data_ready": 6102,
      "mesh_ready": 2270,
      "mesh_empty": 3129,
      "gpu_chunks": 2270,
      "quads": 508896,
      "draws": 1279
    }
  },
  "worst": [
    {
      "frame": 3231,
      "t_s": 2.549,
      "ms": 13.6397,
      "crossed": false,
      "generated": 10,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 79,
      "in_flight": 12
    },
    {
      "frame": 3230,
      "t_s": 2.535,
      "ms": 12.156,
      "crossed": false,
      "generated": 0,
      "meshes": 5,
      "removed": 0,
      "upload_quads": 2677,
      "queued": 91,
      "in_flight": 0
    },
    {
      "frame": 3286,
      "t_s": 2.637,
      "ms": 11.6541,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 1846,
      "t_s": 1.504,
      "ms": 10.8209,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 198,
      "in_flight": 12
    },
    {
      "frame": 3197,
      "t_s": 2.482,
      "ms": 10.7503,
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
      "frames": 1321,
      "fps": 1319.9,
      "worst_ms": 3.1489,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 1103,
      "fps": 1103.6,
      "worst_ms": 10.8209,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 1280,
      "fps": 1279.9,
      "worst_ms": 13.6397,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 1528,
      "fps": 1528.5,
      "worst_ms": 10.2553,
      "generated": 210,
      "meshes": 76,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 2358,
      "fps": 2357.6,
      "worst_ms": 2.4626,
      "generated": 210,
      "meshes": 81,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 2334,
      "fps": 2334.3,
      "worst_ms": 2.8683,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 2355,
      "fps": 2354.6,
      "worst_ms": 3.4199,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 2370,
      "fps": 2370.1,
      "worst_ms": 2.4392,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 2423,
      "fps": 2422.9,
      "worst_ms": 3.4598,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 2406,
      "fps": 2405.9,
      "worst_ms": 2.6846,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 2367,
      "fps": 2367.1,
      "worst_ms": 2.52,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 2036,
      "fps": 2035.6,
      "worst_ms": 2.3246,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 1353,
      "fps": 1353.4,
      "worst_ms": 4.111,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 1285,
      "fps": 1285,
      "worst_ms": 3.5527,
      "generated": 210,
      "meshes": 78,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 2230,
      "fps": 2229.6,
      "worst_ms": 2.7723,
      "generated": 210,
      "meshes": 73,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 2179,
      "fps": 2178.8,
      "worst_ms": 2.7308,
      "generated": 210,
      "meshes": 71,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 2238,
      "fps": 2238,
      "worst_ms": 3.0052,
      "generated": 210,
      "meshes": 75,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 2013,
      "fps": 2013.1,
      "worst_ms": 2.9058,
      "generated": 210,
      "meshes": 72,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 2072,
      "fps": 2072.6,
      "worst_ms": 3.6024,
      "generated": 210,
      "meshes": 77,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 1391,
      "fps": 1391,
      "worst_ms": 3.6471,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    }
  ]
}
```
