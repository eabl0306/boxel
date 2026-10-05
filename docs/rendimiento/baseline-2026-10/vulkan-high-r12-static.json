# boxel stress: static r12

- Run: 2026-10-05 13:58:32 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 2.16 -> 2.68.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 12, settled in 0.86 s with 3270 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 139302 | 6965 | 0.12 | 0.29 | 0.46 | 2.68 | 8.85 | 871 | 1 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 0 | - | - | - |
| other | 139302 | 0.12 | 0.46 | 8.85 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 0 | 0 | 0 | 0 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.0 | 0 | 0 | 0 | 0 |
| jobs in flight | 0.0 | 0 | 0 | 0 | 0 |
| chunks generating or meshing | 0.0 | 0 | 0 | 0 | 0 |

Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 3270 chunks, 1096 meshes (1550 empty), 1096 on the GPU, 239515 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 80429 | 9.66 | 8.85 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 110174 | 15.05 | 4.56 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 80529 | 9.71 | 4.23 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 110223 | 15.08 | 4.07 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 24992 | 2.67 | 3.92 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 9314 | 9312 | 3.81 | 0 | 0 | 0 | 0 | 0 |
| 1 | 9358 | 9358 | 3.91 | 0 | 0 | 0 | 0 | 0 |
| 2 | 9378 | 9380 | 3.92 | 0 | 0 | 0 | 0 | 0 |
| 3 | 9462 | 9462 | 3.76 | 0 | 0 | 0 | 0 | 0 |
| 4 | 9384 | 9384 | 3.78 | 0 | 0 | 0 | 0 | 0 |
| 5 | 9497 | 9497 | 3.77 | 0 | 0 | 0 | 0 | 0 |
| 6 | 9166 | 9164 | 3.80 | 0 | 0 | 0 | 0 | 0 |
| 7 | 5654 | 5655 | 3.78 | 0 | 0 | 0 | 0 | 0 |
| 8 | 5338 | 5338 | 3.11 | 0 | 0 | 0 | 0 | 0 |
| 9 | 5269 | 5269 | 8.85 | 0 | 0 | 0 | 0 | 0 |
| 10 | 5205 | 5205 | 2.94 | 0 | 0 | 0 | 0 | 0 |
| 11 | 5603 | 5603 | 3.70 | 0 | 0 | 0 | 0 | 0 |
| 12 | 5717 | 5717 | 3.67 | 0 | 0 | 0 | 0 | 0 |
| 13 | 5945 | 5944 | 2.79 | 0 | 0 | 0 | 0 | 0 |
| 14 | 5727 | 5726 | 3.09 | 0 | 0 | 0 | 0 | 0 |
| 15 | 5561 | 5562 | 4.56 | 0 | 0 | 0 | 0 | 0 |
| 16 | 5738 | 5738 | 3.49 | 0 | 0 | 0 | 0 | 0 |
| 17 | 6067 | 6066 | 3.84 | 0 | 0 | 0 | 0 | 0 |
| 18 | 5885 | 5885 | 2.99 | 0 | 0 | 0 | 0 | 0 |
| 19 | 6034 | 6034 | 3.32 | 0 | 0 | 0 | 0 | 0 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "static",
    "label": "e933486",
    "radius": 12,
    "speed": 0,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.86,
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
    "load_start": 2.16,
    "load_end": 2.68,
    "unix_time": 1791208712,
    "date_utc": "2026-10-05 13:58:32 UTC",
    "frames_presented": 2865
  },
  "frames": {
    "count": 139302,
    "seconds": 20,
    "avg_fps": 6965.1,
    "mean_ms": 0.1436,
    "p50_ms": 0.1173,
    "p95_ms": 0.2903,
    "p99_ms": 0.4605,
    "p999_ms": 2.679,
    "min_ms": 0.0649,
    "max_ms": 8.8512,
    "low_1_fps": 870.8,
    "over_144hz": 1,
    "over_60hz": 0,
    "crossing": null,
    "other": {
      "frames": 139302,
      "p50_ms": 0.1173,
      "p99_ms": 0.4605,
      "max_ms": 8.8512
    }
  },
  "world": {
    "generated": 0,
    "generated_per_s": 0,
    "meshes": 0,
    "meshes_per_s": 0,
    "removed": 0,
    "removed_per_s": 0,
    "jobs": 0,
    "jobs_per_s": 0,
    "upload_quads": 0,
    "upload_quads_per_s": 0,
    "crossings": 0,
    "queued": {
      "avg": 0,
      "p95": 0,
      "max": 0,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0,
      "p95": 0,
      "max": 0,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 0,
      "p95": 0,
      "max": 0,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": null,
    "catch_up_s": 0.001,
    "final": {
      "chunks": 3270,
      "data_ready": 3270,
      "mesh_ready": 1096,
      "mesh_empty": 1550,
      "gpu_chunks": 1096,
      "quads": 239515,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 80429,
      "t_s": 9.659,
      "ms": 8.8512,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 110174,
      "t_s": 15.053,
      "ms": 4.559,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 80529,
      "t_s": 9.709,
      "ms": 4.2331,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 110223,
      "t_s": 15.082,
      "ms": 4.0658,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 24992,
      "t_s": 2.674,
      "ms": 3.9213,
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
      "frames": 9314,
      "fps": 9312,
      "worst_ms": 3.8059,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 1,
      "frames": 9358,
      "fps": 9358.3,
      "worst_ms": 3.9116,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 2,
      "frames": 9378,
      "fps": 9379.7,
      "worst_ms": 3.9213,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 3,
      "frames": 9462,
      "fps": 9461.9,
      "worst_ms": 3.76,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 4,
      "frames": 9384,
      "fps": 9383.7,
      "worst_ms": 3.7757,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 5,
      "frames": 9497,
      "fps": 9496.8,
      "worst_ms": 3.7716,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 6,
      "frames": 9166,
      "fps": 9164.3,
      "worst_ms": 3.8037,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 7,
      "frames": 5654,
      "fps": 5654.9,
      "worst_ms": 3.7827,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 8,
      "frames": 5338,
      "fps": 5338.5,
      "worst_ms": 3.1132,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 9,
      "frames": 5269,
      "fps": 5268.5,
      "worst_ms": 8.8512,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 10,
      "frames": 5205,
      "fps": 5205,
      "worst_ms": 2.9438,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 11,
      "frames": 5603,
      "fps": 5603.4,
      "worst_ms": 3.7002,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 12,
      "frames": 5717,
      "fps": 5717.1,
      "worst_ms": 3.674,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 13,
      "frames": 5945,
      "fps": 5944.5,
      "worst_ms": 2.7871,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 14,
      "frames": 5727,
      "fps": 5725.9,
      "worst_ms": 3.0858,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 15,
      "frames": 5561,
      "fps": 5562.3,
      "worst_ms": 4.559,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 16,
      "frames": 5738,
      "fps": 5738.4,
      "worst_ms": 3.4923,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 17,
      "frames": 6067,
      "fps": 6066.4,
      "worst_ms": 3.8366,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 18,
      "frames": 5885,
      "fps": 5884.8,
      "worst_ms": 2.9855,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 19,
      "frames": 6034,
      "fps": 6034,
      "worst_ms": 3.3154,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    }
  ]
}
```
