# boxel stress: fly r12 64 vox/s

- Run: 2026-10-05 13:59:24 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 3.43 -> 3.28.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 12, settled in 0.52 s with 3270 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 169596 | 8480 | 0.09 | 0.23 | 0.41 | 2.46 | 6.62 | 1028 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 1.24 | 2.07 | 2.07 |
| other | 169556 | 0.09 | 0.41 | 6.62 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 324 | 119 | 111 | 548 | 27135 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.8 | 0 | 152 | 0 | 0 |
| jobs in flight | 0.1 | 0 | 12 | 0 | 0 |
| chunks generating or meshing | 1.6 | 0 | 287 | 0 | 0 |

After a chunk border the world finished the new chunks in 10 ms on average, 17 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 3690 chunks, 1256 meshes (1882 empty), 1256 on the GPU, 297197 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 45203 | 4.96 | 6.62 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 89621 | 9.72 | 4.39 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 2112 | 0.23 | 4.30 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 139289 | 15.17 | 4.24 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 32893 | 3.61 | 4.16 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 8530 | 8530 | 4.30 | 324 | 132 | 2 | 151 | 12 |
| 1 | 9307 | 9306 | 3.88 | 324 | 117 | 2 | 152 | 12 |
| 2 | 9340 | 9341 | 3.86 | 324 | 119 | 2 | 152 | 12 |
| 3 | 9163 | 9161 | 4.16 | 324 | 120 | 2 | 152 | 12 |
| 4 | 9215 | 9218 | 6.62 | 324 | 122 | 2 | 151 | 12 |
| 5 | 9366 | 9366 | 3.76 | 324 | 127 | 2 | 152 | 12 |
| 6 | 9270 | 9270 | 3.89 | 324 | 127 | 2 | 152 | 12 |
| 7 | 9187 | 9187 | 3.87 | 324 | 123 | 2 | 152 | 12 |
| 8 | 9489 | 9489 | 3.86 | 324 | 118 | 2 | 151 | 12 |
| 9 | 9322 | 9322 | 4.39 | 324 | 114 | 2 | 151 | 12 |
| 10 | 8908 | 8908 | 3.96 | 324 | 114 | 2 | 152 | 12 |
| 11 | 8974 | 8974 | 3.81 | 324 | 118 | 2 | 152 | 12 |
| 12 | 9127 | 9127 | 3.70 | 324 | 118 | 2 | 151 | 12 |
| 13 | 9158 | 9158 | 3.88 | 324 | 109 | 2 | 150 | 12 |
| 14 | 9283 | 9284 | 3.77 | 324 | 110 | 2 | 150 | 12 |
| 15 | 9095 | 9094 | 4.24 | 324 | 112 | 2 | 150 | 12 |
| 16 | 5703 | 5703 | 3.03 | 324 | 112 | 2 | 151 | 12 |
| 17 | 5797 | 5798 | 2.88 | 324 | 121 | 2 | 152 | 12 |
| 18 | 5708 | 5707 | 2.98 | 324 | 127 | 2 | 152 | 12 |
| 19 | 5654 | 5654 | 3.38 | 324 | 122 | 2 | 152 | 12 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "e933486",
    "radius": 12,
    "speed": 64,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.515,
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
    "load_start": 3.43,
    "load_end": 3.28,
    "unix_time": 1791208764,
    "date_utc": "2026-10-05 13:59:24 UTC",
    "frames_presented": 2866
  },
  "frames": {
    "count": 169596,
    "seconds": 20,
    "avg_fps": 8479.8,
    "mean_ms": 0.1179,
    "p50_ms": 0.0934,
    "p95_ms": 0.228,
    "p99_ms": 0.4129,
    "p999_ms": 2.4554,
    "min_ms": 0.0639,
    "max_ms": 6.6162,
    "low_1_fps": 1028.3,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 1.2378,
      "p99_ms": 2.0712,
      "max_ms": 2.0712
    },
    "other": {
      "frames": 169556,
      "p50_ms": 0.0934,
      "p99_ms": 0.4111,
      "max_ms": 6.6162
    }
  },
  "world": {
    "generated": 6480,
    "generated_per_s": 324,
    "meshes": 2382,
    "meshes_per_s": 119.1,
    "removed": 2222,
    "removed_per_s": 111.1,
    "jobs": 10967,
    "jobs_per_s": 548.3,
    "upload_quads": 542708,
    "upload_quads_per_s": 27135.2,
    "crossings": 40,
    "queued": {
      "avg": 0.8,
      "p95": 0,
      "max": 152,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.1,
      "p95": 0,
      "max": 12,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 1.6,
      "p95": 0,
      "max": 287,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 40,
      "finished": 40,
      "avg_ms": 9.6467,
      "max_ms": 17.1467
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 3690,
      "data_ready": 3690,
      "mesh_ready": 1256,
      "mesh_empty": 1882,
      "gpu_chunks": 1256,
      "quads": 297197,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 45203,
      "t_s": 4.959,
      "ms": 6.6162,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 89621,
      "t_s": 9.719,
      "ms": 4.3938,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 2112,
      "t_s": 0.228,
      "ms": 4.2996,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 139289,
      "t_s": 15.169,
      "ms": 4.2432,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 32893,
      "t_s": 3.605,
      "ms": 4.1603,
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
      "frames": 8530,
      "fps": 8529.5,
      "worst_ms": 4.2996,
      "generated": 324,
      "meshes": 132,
      "crossings": 2,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 9307,
      "fps": 9306.5,
      "worst_ms": 3.8845,
      "generated": 324,
      "meshes": 117,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 9340,
      "fps": 9341,
      "worst_ms": 3.864,
      "generated": 324,
      "meshes": 119,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 9163,
      "fps": 9160.5,
      "worst_ms": 4.1603,
      "generated": 324,
      "meshes": 120,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 9215,
      "fps": 9217.6,
      "worst_ms": 6.6162,
      "generated": 324,
      "meshes": 122,
      "crossings": 2,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 9366,
      "fps": 9365.8,
      "worst_ms": 3.7602,
      "generated": 324,
      "meshes": 127,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 9270,
      "fps": 9269.6,
      "worst_ms": 3.8937,
      "generated": 324,
      "meshes": 127,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 9187,
      "fps": 9187.1,
      "worst_ms": 3.8734,
      "generated": 324,
      "meshes": 123,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 9489,
      "fps": 9489,
      "worst_ms": 3.8571,
      "generated": 324,
      "meshes": 118,
      "crossings": 2,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 9322,
      "fps": 9321.7,
      "worst_ms": 4.3938,
      "generated": 324,
      "meshes": 114,
      "crossings": 2,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 8908,
      "fps": 8907.6,
      "worst_ms": 3.961,
      "generated": 324,
      "meshes": 114,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 8974,
      "fps": 8974,
      "worst_ms": 3.806,
      "generated": 324,
      "meshes": 118,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 9127,
      "fps": 9127.1,
      "worst_ms": 3.6987,
      "generated": 324,
      "meshes": 118,
      "crossings": 2,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 9158,
      "fps": 9158.3,
      "worst_ms": 3.877,
      "generated": 324,
      "meshes": 109,
      "crossings": 2,
      "queued_max": 150,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 9283,
      "fps": 9283.7,
      "worst_ms": 3.7678,
      "generated": 324,
      "meshes": 110,
      "crossings": 2,
      "queued_max": 150,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 9095,
      "fps": 9093.8,
      "worst_ms": 4.2432,
      "generated": 324,
      "meshes": 112,
      "crossings": 2,
      "queued_max": 150,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 5703,
      "fps": 5703.4,
      "worst_ms": 3.0305,
      "generated": 324,
      "meshes": 112,
      "crossings": 2,
      "queued_max": 151,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 5797,
      "fps": 5797.7,
      "worst_ms": 2.8809,
      "generated": 324,
      "meshes": 121,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 5708,
      "fps": 5706.9,
      "worst_ms": 2.9821,
      "generated": 324,
      "meshes": 127,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 5654,
      "fps": 5654.2,
      "worst_ms": 3.3836,
      "generated": 324,
      "meshes": 122,
      "crossings": 2,
      "queued_max": 152,
      "in_flight_max": 12
    }
  ]
}
```
