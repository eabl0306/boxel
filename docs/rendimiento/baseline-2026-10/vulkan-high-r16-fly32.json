# boxel stress: fly r16 32 vox/s

- Run: 2026-10-05 14:00:42 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 5.36 -> 4.40.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.89 s with 5598 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 153052 | 7653 | 0.11 | 0.24 | 0.49 | 2.34 | 4.46 | 959 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 1.64 | 3.35 | 3.35 |
| other | 153032 | 0.11 | 0.48 | 4.46 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 210 | 79 | 63 | 360 | 18092 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.7 | 0 | 200 | 0 | 0 |
| jobs in flight | 0.1 | 0 | 12 | 0 | 0 |
| chunks generating or meshing | 1.5 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 13 ms on average, 15 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 6102 chunks, 2270 meshes (3129 empty), 2270 on the GPU, 508896 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 92694 | 11.82 | 4.46 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 109731 | 14.15 | 4.33 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 114117 | 14.76 | 4.31 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 60808 | 7.88 | 4.09 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 16400 | 2.31 | 4.06 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 6384 | 6383 | 3.20 | 210 | 79 | 1 | 198 | 12 |
| 1 | 7590 | 7591 | 3.74 | 210 | 80 | 1 | 200 | 12 |
| 2 | 7872 | 7872 | 4.06 | 210 | 83 | 1 | 198 | 12 |
| 3 | 7966 | 7966 | 3.82 | 210 | 76 | 1 | 200 | 12 |
| 4 | 7999 | 8000 | 3.87 | 210 | 81 | 1 | 200 | 12 |
| 5 | 8072 | 8072 | 3.89 | 210 | 84 | 1 | 200 | 12 |
| 6 | 7995 | 7995 | 3.79 | 210 | 83 | 1 | 199 | 12 |
| 7 | 7909 | 7905 | 4.09 | 210 | 83 | 1 | 200 | 12 |
| 8 | 8099 | 8102 | 3.75 | 210 | 83 | 1 | 200 | 12 |
| 9 | 8065 | 8065 | 3.93 | 210 | 84 | 1 | 200 | 12 |
| 10 | 8068 | 8069 | 3.81 | 210 | 84 | 1 | 200 | 12 |
| 11 | 8117 | 8117 | 4.46 | 210 | 79 | 1 | 199 | 12 |
| 12 | 7472 | 7472 | 3.74 | 210 | 80 | 1 | 200 | 12 |
| 13 | 7028 | 7027 | 3.35 | 210 | 78 | 1 | 200 | 12 |
| 14 | 7163 | 7164 | 4.33 | 210 | 73 | 1 | 200 | 12 |
| 15 | 7422 | 7422 | 3.96 | 210 | 71 | 1 | 200 | 12 |
| 16 | 7087 | 7087 | 4.05 | 210 | 75 | 1 | 200 | 12 |
| 17 | 7180 | 7180 | 3.62 | 210 | 72 | 1 | 200 | 12 |
| 18 | 7478 | 7479 | 3.44 | 210 | 77 | 1 | 199 | 12 |
| 19 | 8086 | 8086 | 3.79 | 210 | 79 | 1 | 200 | 12 |

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
    "settle_s": 0.891,
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
    "load_start": 5.36,
    "load_end": 4.4,
    "unix_time": 1791208842,
    "date_utc": "2026-10-05 14:00:42 UTC",
    "frames_presented": 2869
  },
  "frames": {
    "count": 153052,
    "seconds": 20,
    "avg_fps": 7652.6,
    "mean_ms": 0.1307,
    "p50_ms": 0.1069,
    "p95_ms": 0.2437,
    "p99_ms": 0.4853,
    "p999_ms": 2.3353,
    "min_ms": 0.0702,
    "max_ms": 4.4635,
    "low_1_fps": 959.1,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 1.6442,
      "p99_ms": 3.3506,
      "max_ms": 3.3506
    },
    "other": {
      "frames": 153032,
      "p50_ms": 0.1069,
      "p99_ms": 0.4822,
      "max_ms": 4.4635
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
      "avg": 0.7,
      "p95": 0,
      "max": 200,
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
      "avg": 1.5,
      "p95": 0,
      "max": 374,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "avg_ms": 12.6907,
      "max_ms": 15.432
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
      "frame": 92694,
      "t_s": 11.82,
      "ms": 4.4635,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 109731,
      "t_s": 14.153,
      "ms": 4.3286,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 114117,
      "t_s": 14.764,
      "ms": 4.3129,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 60808,
      "t_s": 7.877,
      "ms": 4.0929,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 16400,
      "t_s": 2.308,
      "ms": 4.0565,
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
      "frames": 6384,
      "fps": 6383.2,
      "worst_ms": 3.1999,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 198,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 7590,
      "fps": 7590.7,
      "worst_ms": 3.7441,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 7872,
      "fps": 7871.5,
      "worst_ms": 4.0565,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 198,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 7966,
      "fps": 7965.6,
      "worst_ms": 3.8161,
      "generated": 210,
      "meshes": 76,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 7999,
      "fps": 7999.9,
      "worst_ms": 3.8699,
      "generated": 210,
      "meshes": 81,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 8072,
      "fps": 8071.9,
      "worst_ms": 3.8926,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 7995,
      "fps": 7995.3,
      "worst_ms": 3.7923,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 7909,
      "fps": 7905.2,
      "worst_ms": 4.0929,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 8099,
      "fps": 8102.1,
      "worst_ms": 3.7543,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 8065,
      "fps": 8065,
      "worst_ms": 3.9267,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 8068,
      "fps": 8069.1,
      "worst_ms": 3.8078,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 8117,
      "fps": 8116.5,
      "worst_ms": 4.4635,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 7472,
      "fps": 7472.2,
      "worst_ms": 3.7367,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 7028,
      "fps": 7026.8,
      "worst_ms": 3.3506,
      "generated": 210,
      "meshes": 78,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 7163,
      "fps": 7163.8,
      "worst_ms": 4.3286,
      "generated": 210,
      "meshes": 73,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 7422,
      "fps": 7421.9,
      "worst_ms": 3.9648,
      "generated": 210,
      "meshes": 71,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 7087,
      "fps": 7087.1,
      "worst_ms": 4.0522,
      "generated": 210,
      "meshes": 75,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 7180,
      "fps": 7179.7,
      "worst_ms": 3.6215,
      "generated": 210,
      "meshes": 72,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 7478,
      "fps": 7478.6,
      "worst_ms": 3.4384,
      "generated": 210,
      "meshes": 77,
      "crossings": 1,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 8086,
      "fps": 8085.6,
      "worst_ms": 3.787,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 200,
      "in_flight_max": 12
    }
  ]
}
```
