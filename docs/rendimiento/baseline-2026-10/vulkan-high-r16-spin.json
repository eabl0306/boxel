# boxel stress: spin r16

- Run: 2026-10-05 14:01:35 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 4.88 -> 4.42.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.90 s with 5598 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 134077 | 6704 | 0.12 | 0.29 | 0.45 | 2.49 | 5.39 | 959 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 0 | - | - | - |
| other | 134077 | 0.12 | 0.45 | 5.39 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 0 | 0 | 0 | 0 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.0 | 0 | 0 | 0 | 0 |
| jobs in flight | 0.0 | 0 | 0 | 0 | 0 |
| chunks generating or meshing | 0.0 | 0 | 0 | 0 | 0 |

Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 5598 chunks, 1951 meshes (2831 empty), 1951 on the GPU, 439764 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 60330 | 10.34 | 5.39 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 9081 | 1.64 | 5.29 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 60551 | 10.39 | 3.94 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 63127 | 10.73 | 3.90 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 115439 | 17.53 | 3.89 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 5819 | 5819 | 2.85 | 0 | 0 | 0 | 0 | 0 |
| 1 | 4575 | 4574 | 5.29 | 0 | 0 | 0 | 0 | 0 |
| 2 | 5078 | 5078 | 3.21 | 0 | 0 | 0 | 0 | 0 |
| 3 | 5682 | 5682 | 2.95 | 0 | 0 | 0 | 0 | 0 |
| 4 | 5668 | 5669 | 3.13 | 0 | 0 | 0 | 0 | 0 |
| 5 | 7109 | 7109 | 3.26 | 0 | 0 | 0 | 0 | 0 |
| 6 | 7223 | 7223 | 3.69 | 0 | 0 | 0 | 0 | 0 |
| 7 | 5869 | 5869 | 2.96 | 0 | 0 | 0 | 0 | 0 |
| 8 | 5957 | 5957 | 3.05 | 0 | 0 | 0 | 0 | 0 |
| 9 | 5321 | 5321 | 3.49 | 0 | 0 | 0 | 0 | 0 |
| 10 | 6749 | 6749 | 5.39 | 0 | 0 | 0 | 0 | 0 |
| 11 | 7367 | 7367 | 3.85 | 0 | 0 | 0 | 0 | 0 |
| 12 | 7817 | 7817 | 3.65 | 0 | 0 | 0 | 0 | 0 |
| 13 | 7550 | 7550 | 3.81 | 0 | 0 | 0 | 0 | 0 |
| 14 | 7885 | 7885 | 3.79 | 0 | 0 | 0 | 0 | 0 |
| 15 | 7983 | 7983 | 3.81 | 0 | 0 | 0 | 0 | 0 |
| 16 | 7698 | 7698 | 3.86 | 0 | 0 | 0 | 0 | 0 |
| 17 | 7672 | 7672 | 3.89 | 0 | 0 | 0 | 0 | 0 |
| 18 | 7521 | 7521 | 3.58 | 0 | 0 | 0 | 0 | 0 |
| 19 | 7534 | 7534 | 3.71 | 0 | 0 | 0 | 0 | 0 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "spin",
    "label": "e933486",
    "radius": 16,
    "speed": 0,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.897,
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
    "load_start": 4.88,
    "load_end": 4.42,
    "unix_time": 1791208895,
    "date_utc": "2026-10-05 14:01:35 UTC",
    "frames_presented": 2867
  },
  "frames": {
    "count": 134077,
    "seconds": 20,
    "avg_fps": 6703.8,
    "mean_ms": 0.1492,
    "p50_ms": 0.1204,
    "p95_ms": 0.2852,
    "p99_ms": 0.4505,
    "p999_ms": 2.4863,
    "min_ms": 0.0714,
    "max_ms": 5.3924,
    "low_1_fps": 959.5,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": null,
    "other": {
      "frames": 134077,
      "p50_ms": 0.1204,
      "p99_ms": 0.4505,
      "max_ms": 5.3924
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
    "catch_up_s": 0,
    "final": {
      "chunks": 5598,
      "data_ready": 5598,
      "mesh_ready": 1951,
      "mesh_empty": 2831,
      "gpu_chunks": 1951,
      "quads": 439764,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 60330,
      "t_s": 10.336,
      "ms": 5.3924,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 9081,
      "t_s": 1.643,
      "ms": 5.2861,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 60551,
      "t_s": 10.388,
      "ms": 3.9366,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 63127,
      "t_s": 10.732,
      "ms": 3.8986,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 115439,
      "t_s": 17.53,
      "ms": 3.8872,
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
      "frames": 5819,
      "fps": 5818.9,
      "worst_ms": 2.8482,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 1,
      "frames": 4575,
      "fps": 4574.4,
      "worst_ms": 5.2861,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 2,
      "frames": 5078,
      "fps": 5078.3,
      "worst_ms": 3.2108,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 3,
      "frames": 5682,
      "fps": 5681.6,
      "worst_ms": 2.9536,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 4,
      "frames": 5668,
      "fps": 5668.7,
      "worst_ms": 3.131,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 5,
      "frames": 7109,
      "fps": 7109.3,
      "worst_ms": 3.259,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 6,
      "frames": 7223,
      "fps": 7222.8,
      "worst_ms": 3.6911,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 7,
      "frames": 5869,
      "fps": 5869,
      "worst_ms": 2.961,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 8,
      "frames": 5957,
      "fps": 5956.7,
      "worst_ms": 3.0488,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 9,
      "frames": 5321,
      "fps": 5321,
      "worst_ms": 3.4855,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 10,
      "frames": 6749,
      "fps": 6749.1,
      "worst_ms": 5.3924,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 11,
      "frames": 7367,
      "fps": 7366.9,
      "worst_ms": 3.8545,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 12,
      "frames": 7817,
      "fps": 7816.9,
      "worst_ms": 3.6535,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 13,
      "frames": 7550,
      "fps": 7550.2,
      "worst_ms": 3.8078,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 14,
      "frames": 7885,
      "fps": 7885.3,
      "worst_ms": 3.7911,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 15,
      "frames": 7983,
      "fps": 7982.8,
      "worst_ms": 3.8142,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 16,
      "frames": 7698,
      "fps": 7697.6,
      "worst_ms": 3.8567,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 17,
      "frames": 7672,
      "fps": 7672.3,
      "worst_ms": 3.8872,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 18,
      "frames": 7521,
      "fps": 7521.1,
      "worst_ms": 3.5794,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 19,
      "frames": 7534,
      "fps": 7534.1,
      "worst_ms": 3.7147,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    }
  ]
}
```
