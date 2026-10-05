# boxel stress: fly r16 64 vox/s

- Run: 2026-10-05 14:01:08 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 4.04 -> 4.66.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.59 s with 5598 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 126970 | 6348 | 0.13 | 0.31 | 0.46 | 2.66 | 5.45 | 920 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 2.14 | 2.84 | 2.84 |
| other | 126930 | 0.13 | 0.46 | 5.45 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 420 | 158 | 145 | 717 | 35898 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 1.7 | 0 | 200 | 0 | 0 |
| jobs in flight | 0.1 | 0 | 12 | 0 | 0 |
| chunks generating or meshing | 3.6 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 17 ms on average, 22 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 6102 chunks, 2200 meshes (3199 empty), 2200 on the GPU, 509198 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 103531 | 15.53 | 5.45 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 63094 | 7.96 | 4.37 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 624 | 0.08 | 4.11 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 50863 | 6.40 | 4.07 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 51842 | 6.53 | 3.97 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 7729 | 7727 | 4.11 | 420 | 159 | 2 | 199 | 12 |
| 1 | 8074 | 8076 | 3.87 | 420 | 159 | 2 | 198 | 12 |
| 2 | 8012 | 8012 | 3.83 | 420 | 165 | 2 | 200 | 12 |
| 3 | 7799 | 7798 | 3.92 | 420 | 166 | 2 | 200 | 12 |
| 4 | 7972 | 7972 | 3.81 | 420 | 167 | 2 | 200 | 12 |
| 5 | 7962 | 7962 | 3.87 | 420 | 163 | 2 | 199 | 12 |
| 6 | 8075 | 8075 | 4.07 | 420 | 158 | 2 | 200 | 12 |
| 7 | 7727 | 7727 | 4.37 | 420 | 144 | 2 | 200 | 12 |
| 8 | 5923 | 5923 | 3.84 | 420 | 147 | 2 | 200 | 12 |
| 9 | 5065 | 5065 | 3.02 | 420 | 156 | 2 | 200 | 12 |
| 10 | 5205 | 5205 | 3.82 | 420 | 152 | 2 | 199 | 12 |
| 11 | 5020 | 5020 | 2.89 | 420 | 151 | 2 | 198 | 12 |
| 12 | 5317 | 5317 | 2.91 | 420 | 153 | 2 | 198 | 12 |
| 13 | 5184 | 5182 | 3.11 | 420 | 150 | 2 | 198 | 12 |
| 14 | 5527 | 5528 | 3.25 | 420 | 151 | 2 | 199 | 12 |
| 15 | 5300 | 5300 | 5.45 | 420 | 157 | 2 | 200 | 12 |
| 16 | 5185 | 5185 | 3.05 | 420 | 159 | 2 | 200 | 12 |
| 17 | 5259 | 5259 | 3.46 | 420 | 159 | 2 | 200 | 12 |
| 18 | 5458 | 5457 | 3.38 | 420 | 166 | 2 | 200 | 12 |
| 19 | 5177 | 5178 | 3.03 | 420 | 172 | 2 | 200 | 12 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "e933486",
    "radius": 16,
    "speed": 64,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.589,
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
    "load_start": 4.04,
    "load_end": 4.66,
    "unix_time": 1791208868,
    "date_utc": "2026-10-05 14:01:08 UTC",
    "frames_presented": 2870
  },
  "frames": {
    "count": 126970,
    "seconds": 20,
    "avg_fps": 6348.5,
    "mean_ms": 0.1575,
    "p50_ms": 0.1317,
    "p95_ms": 0.3096,
    "p99_ms": 0.4602,
    "p999_ms": 2.6568,
    "min_ms": 0.0712,
    "max_ms": 5.4483,
    "low_1_fps": 920.2,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 2.1381,
      "p99_ms": 2.8427,
      "max_ms": 2.8427
    },
    "other": {
      "frames": 126930,
      "p50_ms": 0.1317,
      "p99_ms": 0.4565,
      "max_ms": 5.4483
    }
  },
  "world": {
    "generated": 8400,
    "generated_per_s": 420,
    "meshes": 3154,
    "meshes_per_s": 157.7,
    "removed": 2905,
    "removed_per_s": 145.2,
    "jobs": 14332,
    "jobs_per_s": 716.6,
    "upload_quads": 717955,
    "upload_quads_per_s": 35897.7,
    "crossings": 40,
    "queued": {
      "avg": 1.7,
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
      "avg": 3.6,
      "p95": 0,
      "max": 374,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 40,
      "finished": 40,
      "avg_ms": 16.5538,
      "max_ms": 22.3853
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 6102,
      "data_ready": 6102,
      "mesh_ready": 2200,
      "mesh_empty": 3199,
      "gpu_chunks": 2200,
      "quads": 509198,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 103531,
      "t_s": 15.529,
      "ms": 5.4483,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 63094,
      "t_s": 7.96,
      "ms": 4.3675,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 624,
      "t_s": 0.08,
      "ms": 4.109,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 50863,
      "t_s": 6.402,
      "ms": 4.0717,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 51842,
      "t_s": 6.525,
      "ms": 3.9681,
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
      "frames": 7729,
      "fps": 7726.7,
      "worst_ms": 4.109,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 1,
      "frames": 8074,
      "fps": 8076.3,
      "worst_ms": 3.8661,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 198,
      "in_flight_max": 12
    },
    {
      "second": 2,
      "frames": 8012,
      "fps": 8011.8,
      "worst_ms": 3.834,
      "generated": 420,
      "meshes": 165,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 3,
      "frames": 7799,
      "fps": 7798.4,
      "worst_ms": 3.9204,
      "generated": 420,
      "meshes": 166,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 4,
      "frames": 7972,
      "fps": 7972.3,
      "worst_ms": 3.8129,
      "generated": 420,
      "meshes": 167,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 5,
      "frames": 7962,
      "fps": 7961.5,
      "worst_ms": 3.872,
      "generated": 420,
      "meshes": 163,
      "crossings": 2,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 6,
      "frames": 8075,
      "fps": 8075.4,
      "worst_ms": 4.0717,
      "generated": 420,
      "meshes": 158,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 7,
      "frames": 7727,
      "fps": 7727.3,
      "worst_ms": 4.3675,
      "generated": 420,
      "meshes": 144,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 8,
      "frames": 5923,
      "fps": 5922.6,
      "worst_ms": 3.8386,
      "generated": 420,
      "meshes": 147,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 9,
      "frames": 5065,
      "fps": 5065.1,
      "worst_ms": 3.0204,
      "generated": 420,
      "meshes": 156,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 10,
      "frames": 5205,
      "fps": 5204.5,
      "worst_ms": 3.8198,
      "generated": 420,
      "meshes": 152,
      "crossings": 2,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 11,
      "frames": 5020,
      "fps": 5020.4,
      "worst_ms": 2.8871,
      "generated": 420,
      "meshes": 151,
      "crossings": 2,
      "queued_max": 198,
      "in_flight_max": 12
    },
    {
      "second": 12,
      "frames": 5317,
      "fps": 5317.3,
      "worst_ms": 2.9074,
      "generated": 420,
      "meshes": 153,
      "crossings": 2,
      "queued_max": 198,
      "in_flight_max": 12
    },
    {
      "second": 13,
      "frames": 5184,
      "fps": 5182.5,
      "worst_ms": 3.1058,
      "generated": 420,
      "meshes": 150,
      "crossings": 2,
      "queued_max": 198,
      "in_flight_max": 12
    },
    {
      "second": 14,
      "frames": 5527,
      "fps": 5528.2,
      "worst_ms": 3.2529,
      "generated": 420,
      "meshes": 151,
      "crossings": 2,
      "queued_max": 199,
      "in_flight_max": 12
    },
    {
      "second": 15,
      "frames": 5300,
      "fps": 5299.9,
      "worst_ms": 5.4483,
      "generated": 420,
      "meshes": 157,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 16,
      "frames": 5185,
      "fps": 5185.5,
      "worst_ms": 3.0455,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 17,
      "frames": 5259,
      "fps": 5258.8,
      "worst_ms": 3.4619,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 18,
      "frames": 5458,
      "fps": 5457,
      "worst_ms": 3.3838,
      "generated": 420,
      "meshes": 166,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    },
    {
      "second": 19,
      "frames": 5177,
      "fps": 5178.2,
      "worst_ms": 3.0281,
      "generated": 420,
      "meshes": 172,
      "crossings": 2,
      "queued_max": 200,
      "in_flight_max": 12
    }
  ]
}
```
