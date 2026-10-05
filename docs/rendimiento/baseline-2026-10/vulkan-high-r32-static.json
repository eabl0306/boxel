# boxel stress: static r32

- Run: 2026-10-05 14:02:02 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 4.47 -> 4.13.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 1.66 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 69009 | 3450 | 0.25 | 0.62 | 0.87 | 2.83 | 5.22 | 650 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 0 | - | - | - |
| other | 69009 | 0.25 | 0.87 | 5.22 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 0 | 0 | 0 | 0 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.0 | 0 | 0 | 0 | 0 |
| jobs in flight | 0.0 | 0 | 0 | 0 | 0 |
| chunks generating or meshing | 0.0 | 0 | 0 | 0 | 0 |

Queue trend +0.0 jobs/s (above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 20838 chunks, 7794 meshes (11460 empty), 7794 on the GPU, 1760326 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|
| 21603 | 5.95 | 5.22 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 21593 | 5.94 | 5.21 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 21687 | 6.05 | 4.90 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 23088 | 6.54 | 4.80 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 53505 | 15.55 | 4.61 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3652 | 3652 | 3.98 | 0 | 0 | 0 | 0 | 0 |
| 1 | 3628 | 3626 | 4.03 | 0 | 0 | 0 | 0 | 0 |
| 2 | 3652 | 3655 | 4.01 | 0 | 0 | 0 | 0 | 0 |
| 3 | 3597 | 3596 | 3.94 | 0 | 0 | 0 | 0 | 0 |
| 4 | 3670 | 3670 | 3.91 | 0 | 0 | 0 | 0 | 0 |
| 5 | 3445 | 3444 | 5.22 | 0 | 0 | 0 | 0 | 0 |
| 6 | 2983 | 2983 | 4.90 | 0 | 0 | 0 | 0 | 0 |
| 7 | 3381 | 3382 | 3.79 | 0 | 0 | 0 | 0 | 0 |
| 8 | 3431 | 3431 | 3.51 | 0 | 0 | 0 | 0 | 0 |
| 9 | 3365 | 3365 | 4.22 | 0 | 0 | 0 | 0 | 0 |
| 10 | 3387 | 3387 | 3.83 | 0 | 0 | 0 | 0 | 0 |
| 11 | 3383 | 3383 | 3.84 | 0 | 0 | 0 | 0 | 0 |
| 12 | 3350 | 3350 | 3.49 | 0 | 0 | 0 | 0 | 0 |
| 13 | 3409 | 3409 | 3.66 | 0 | 0 | 0 | 0 | 0 |
| 14 | 3339 | 3339 | 4.42 | 0 | 0 | 0 | 0 | 0 |
| 15 | 3350 | 3350 | 4.61 | 0 | 0 | 0 | 0 | 0 |
| 16 | 3488 | 3488 | 3.44 | 0 | 0 | 0 | 0 | 0 |
| 17 | 3496 | 3495 | 3.48 | 0 | 0 | 0 | 0 | 0 |
| 18 | 3496 | 3497 | 4.20 | 0 | 0 | 0 | 0 | 0 |
| 19 | 3507 | 3507 | 4.29 | 0 | 0 | 0 | 0 | 0 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "static",
    "label": "e933486",
    "radius": 32,
    "speed": 0,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 1.662,
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
    "load_start": 4.47,
    "load_end": 4.13,
    "unix_time": 1791208922,
    "date_utc": "2026-10-05 14:02:02 UTC",
    "frames_presented": 2869
  },
  "frames": {
    "count": 69009,
    "seconds": 20,
    "avg_fps": 3450.4,
    "mean_ms": 0.2898,
    "p50_ms": 0.2471,
    "p95_ms": 0.6203,
    "p99_ms": 0.8709,
    "p999_ms": 2.8314,
    "min_ms": 0.0951,
    "max_ms": 5.2203,
    "low_1_fps": 649.9,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": null,
    "other": {
      "frames": 69009,
      "p50_ms": 0.2471,
      "p99_ms": 0.8709,
      "max_ms": 5.2203
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
      "chunks": 20838,
      "data_ready": 20838,
      "mesh_ready": 7794,
      "mesh_empty": 11460,
      "gpu_chunks": 7794,
      "quads": 1760326,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 21603,
      "t_s": 5.951,
      "ms": 5.2203,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 21593,
      "t_s": 5.938,
      "ms": 5.2052,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 21687,
      "t_s": 6.051,
      "ms": 4.8991,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 23088,
      "t_s": 6.543,
      "ms": 4.8008,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 53505,
      "t_s": 15.547,
      "ms": 4.6104,
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
      "frames": 3652,
      "fps": 3651.7,
      "worst_ms": 3.9759,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 1,
      "frames": 3628,
      "fps": 3625.6,
      "worst_ms": 4.0313,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 2,
      "frames": 3652,
      "fps": 3654.5,
      "worst_ms": 4.0111,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 3,
      "frames": 3597,
      "fps": 3596.4,
      "worst_ms": 3.9373,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 4,
      "frames": 3670,
      "fps": 3670.1,
      "worst_ms": 3.9065,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 5,
      "frames": 3445,
      "fps": 3443.9,
      "worst_ms": 5.2203,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 6,
      "frames": 2983,
      "fps": 2983.3,
      "worst_ms": 4.8991,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 7,
      "frames": 3381,
      "fps": 3382.2,
      "worst_ms": 3.7894,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 8,
      "frames": 3431,
      "fps": 3430.7,
      "worst_ms": 3.5116,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 9,
      "frames": 3365,
      "fps": 3365,
      "worst_ms": 4.2247,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 10,
      "frames": 3387,
      "fps": 3386.7,
      "worst_ms": 3.83,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 11,
      "frames": 3383,
      "fps": 3383,
      "worst_ms": 3.8366,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 12,
      "frames": 3350,
      "fps": 3349.8,
      "worst_ms": 3.4873,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 13,
      "frames": 3409,
      "fps": 3409.1,
      "worst_ms": 3.6604,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 14,
      "frames": 3339,
      "fps": 3339.4,
      "worst_ms": 4.4189,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 15,
      "frames": 3350,
      "fps": 3350.3,
      "worst_ms": 4.6104,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 16,
      "frames": 3488,
      "fps": 3487.6,
      "worst_ms": 3.4352,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 17,
      "frames": 3496,
      "fps": 3495.5,
      "worst_ms": 3.4821,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 18,
      "frames": 3496,
      "fps": 3497,
      "worst_ms": 4.2038,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 19,
      "frames": 3507,
      "fps": 3506.6,
      "worst_ms": 4.294,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    }
  ]
}
```
