# boxel stress: fly r32 64 vox/s

- Run: 2026-10-05 14:02:56 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 3.04 -> 2.46.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 1.85 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 72099 | 3605 | 0.24 | 0.52 | 0.71 | 3.57 | 8.96 | 625 | 5 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 5.96 | 8.96 | 8.96 |
| other | 72059 | 0.24 | 0.71 | 6.32 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 804 | 318 | 280 | 1391 | 70073 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 11.6 | 63 | 392 | 0 | 0 |
| jobs in flight | 0.4 | 3 | 12 | 0 | 0 |
| chunks generating or meshing | 23.1 | 163 | 719 | 0 | 0 |

After a chunk border the world finished the new chunks in 39 ms on average, 44 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 21870 chunks, 8549 meshes (12008 empty), 8549 on the GPU, 1881685 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 39714 | 11.25 | 8.96 | yes | 12 | 0 | 148 | 0 | 390 | 12 |
| 56323 | 15.75 | 7.92 | yes | 12 | 0 | 157 | 0 | 390 | 12 |
| 47164 | 13.25 | 7.63 | yes | 12 | 0 | 153 | 0 | 390 | 12 |
| 49026 | 13.75 | 7.47 | yes | 12 | 0 | 156 | 0 | 390 | 12 |
| 15638 | 4.75 | 7.46 | yes | 12 | 0 | 149 | 0 | 390 | 12 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3184 | 3183 | 6.65 | 804 | 316 | 2 | 392 | 12 |
| 1 | 3176 | 3177 | 5.18 | 804 | 316 | 2 | 392 | 12 |
| 2 | 3142 | 3140 | 6.93 | 804 | 310 | 2 | 391 | 12 |
| 3 | 3441 | 3443 | 5.83 | 804 | 316 | 2 | 390 | 12 |
| 4 | 3561 | 3561 | 7.46 | 804 | 301 | 2 | 390 | 12 |
| 5 | 3560 | 3559 | 6.32 | 804 | 300 | 2 | 390 | 12 |
| 6 | 3691 | 3692 | 5.78 | 804 | 303 | 2 | 391 | 12 |
| 7 | 3714 | 3714 | 6.66 | 804 | 321 | 2 | 392 | 12 |
| 8 | 3780 | 3780 | 6.74 | 804 | 328 | 2 | 392 | 12 |
| 9 | 3762 | 3763 | 6.91 | 804 | 334 | 2 | 392 | 12 |
| 10 | 3746 | 3746 | 6.67 | 804 | 318 | 2 | 392 | 12 |
| 11 | 3740 | 3740 | 8.96 | 804 | 310 | 2 | 392 | 12 |
| 12 | 3731 | 3731 | 6.93 | 804 | 320 | 2 | 392 | 12 |
| 13 | 3702 | 3702 | 7.63 | 804 | 308 | 2 | 391 | 12 |
| 14 | 3665 | 3665 | 6.23 | 804 | 326 | 2 | 390 | 12 |
| 15 | 3615 | 3615 | 7.92 | 804 | 335 | 2 | 390 | 12 |
| 16 | 3589 | 3589 | 6.66 | 804 | 329 | 2 | 390 | 12 |
| 17 | 3721 | 3721 | 5.96 | 804 | 326 | 2 | 392 | 12 |
| 18 | 3790 | 3790 | 6.17 | 804 | 330 | 2 | 392 | 12 |
| 19 | 3789 | 3788 | 6.12 | 804 | 318 | 2 | 390 | 12 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "e933486",
    "radius": 32,
    "speed": 64,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 1.855,
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
    "load_start": 3.04,
    "load_end": 2.46,
    "unix_time": 1791208976,
    "date_utc": "2026-10-05 14:02:56 UTC",
    "frames_presented": 2864
  },
  "frames": {
    "count": 72099,
    "seconds": 20,
    "avg_fps": 3604.9,
    "mean_ms": 0.2774,
    "p50_ms": 0.2413,
    "p95_ms": 0.5165,
    "p99_ms": 0.7127,
    "p999_ms": 3.5723,
    "min_ms": 0.0941,
    "max_ms": 8.9571,
    "low_1_fps": 625.2,
    "over_144hz": 5,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 5.9641,
      "p99_ms": 8.9571,
      "max_ms": 8.9571
    },
    "other": {
      "frames": 72059,
      "p50_ms": 0.2413,
      "p99_ms": 0.7051,
      "max_ms": 6.3219
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
    "upload_quads_per_s": 70073,
    "crossings": 40,
    "queued": {
      "avg": 11.6,
      "p95": 63,
      "max": 392,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.4,
      "p95": 3,
      "max": 12,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 23.1,
      "p95": 163,
      "max": 719,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 40,
      "finished": 40,
      "avg_ms": 39.4234,
      "max_ms": 43.6638
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
      "frame": 39714,
      "t_s": 11.25,
      "ms": 8.9571,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 148,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 56323,
      "t_s": 15.75,
      "ms": 7.9172,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 157,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 47164,
      "t_s": 13.25,
      "ms": 7.6314,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 153,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 49026,
      "t_s": 13.75,
      "ms": 7.4721,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 156,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    },
    {
      "frame": 15638,
      "t_s": 4.75,
      "ms": 7.4633,
      "crossed": true,
      "generated": 12,
      "meshes": 0,
      "removed": 149,
      "upload_quads": 0,
      "queued": 390,
      "in_flight": 12
    }
  ],
  "timeline": [
    {
      "second": 0,
      "frames": 3184,
      "fps": 3183.1,
      "worst_ms": 6.6464,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 3176,
      "fps": 3176.5,
      "worst_ms": 5.179,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 3142,
      "fps": 3139.9,
      "worst_ms": 6.9288,
      "generated": 804,
      "meshes": 310,
      "crossings": 2,
      "queued_max": 391,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 3441,
      "fps": 3443.3,
      "worst_ms": 5.8316,
      "generated": 804,
      "meshes": 316,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 3561,
      "fps": 3561.3,
      "worst_ms": 7.4633,
      "generated": 804,
      "meshes": 301,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 3560,
      "fps": 3559.4,
      "worst_ms": 6.3219,
      "generated": 804,
      "meshes": 300,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 3691,
      "fps": 3691.6,
      "worst_ms": 5.7754,
      "generated": 804,
      "meshes": 303,
      "crossings": 2,
      "queued_max": 391,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 3714,
      "fps": 3713.8,
      "worst_ms": 6.6553,
      "generated": 804,
      "meshes": 321,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 3780,
      "fps": 3779.7,
      "worst_ms": 6.7442,
      "generated": 804,
      "meshes": 328,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 3762,
      "fps": 3762.6,
      "worst_ms": 6.9068,
      "generated": 804,
      "meshes": 334,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 3746,
      "fps": 3745.7,
      "worst_ms": 6.6651,
      "generated": 804,
      "meshes": 318,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 3740,
      "fps": 3739.7,
      "worst_ms": 8.9571,
      "generated": 804,
      "meshes": 310,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 3731,
      "fps": 3731.4,
      "worst_ms": 6.928,
      "generated": 804,
      "meshes": 320,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 3702,
      "fps": 3702.1,
      "worst_ms": 7.6314,
      "generated": 804,
      "meshes": 308,
      "crossings": 2,
      "queued_max": 391,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 3665,
      "fps": 3664.9,
      "worst_ms": 6.2328,
      "generated": 804,
      "meshes": 326,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 3615,
      "fps": 3615.1,
      "worst_ms": 7.9172,
      "generated": 804,
      "meshes": 335,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 3589,
      "fps": 3588.9,
      "worst_ms": 6.6621,
      "generated": 804,
      "meshes": 329,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 3721,
      "fps": 3721.2,
      "worst_ms": 5.9641,
      "generated": 804,
      "meshes": 326,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 3790,
      "fps": 3790.2,
      "worst_ms": 6.172,
      "generated": 804,
      "meshes": 330,
      "crossings": 2,
      "queued_max": 392,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 3789,
      "fps": 3788.2,
      "worst_ms": 6.1185,
      "generated": 804,
      "meshes": 318,
      "crossings": 2,
      "queued_max": 390,
      "in_flight_max": 12
    }
  ]
}
```
