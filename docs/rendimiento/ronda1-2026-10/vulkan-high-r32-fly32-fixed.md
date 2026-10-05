# boxel stress: fly r32 32 vox/s

- Run: 2026-10-05 15:38:46 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 4.17 -> 3.34.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 0.71 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 73353 | 3668 | 0.24 | 0.53 | 0.69 | 2.70 | 4.28 | 809 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 1.65 | 2.57 | 2.57 |
| other | 73333 | 0.24 | 0.69 | 4.28 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 402 | 157 | 122 | 694 | 35285 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.7 | 0 | 354 | 0 | 0 |
| jobs in flight | 0.3 | 0 | 102 | 0 | 0 |
| chunks generating or meshing | 1.9 | 0 | 719 | 0 | 0 |

After a chunk border the world finished the new chunks in 9 ms on average, 11 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 21870 chunks, 8496 meshes (12021 empty), 8496 on the GPU, 1904612 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 25918 | 7.20 | 4.28 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 31037 | 8.62 | 4.27 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 8660 | 2.39 | 4.16 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 48751 | 13.44 | 4.15 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 16621 | 4.61 | 4.04 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3617 | 3617 | 3.54 | 402 | 159 | 1 | 336 | 66 |
| 1 | 3615 | 3615 | 3.22 | 402 | 157 | 1 | 342 | 102 |
| 2 | 3591 | 3591 | 4.16 | 402 | 158 | 1 | 348 | 90 |
| 3 | 3582 | 3582 | 3.84 | 402 | 158 | 1 | 272 | 90 |
| 4 | 3585 | 3585 | 4.04 | 402 | 156 | 1 | 342 | 78 |
| 5 | 3607 | 3606 | 4.01 | 402 | 154 | 1 | 298 | 72 |
| 6 | 3614 | 3616 | 3.48 | 402 | 157 | 1 | 284 | 78 |
| 7 | 3583 | 3583 | 4.28 | 402 | 159 | 1 | 336 | 84 |
| 8 | 3635 | 3634 | 4.27 | 402 | 150 | 1 | 266 | 96 |
| 9 | 3636 | 3637 | 3.88 | 402 | 151 | 1 | 335 | 96 |
| 10 | 3654 | 3654 | 3.89 | 402 | 149 | 1 | 336 | 84 |
| 11 | 3678 | 3678 | 3.87 | 402 | 151 | 1 | 270 | 96 |
| 12 | 3698 | 3698 | 3.58 | 402 | 152 | 1 | 342 | 90 |
| 13 | 3723 | 3722 | 4.15 | 402 | 151 | 1 | 336 | 84 |
| 14 | 3737 | 3738 | 3.55 | 402 | 158 | 1 | 279 | 90 |
| 15 | 3721 | 3720 | 3.90 | 402 | 163 | 1 | 348 | 66 |
| 16 | 3758 | 3758 | 3.76 | 402 | 166 | 1 | 348 | 60 |
| 17 | 3796 | 3796 | 2.88 | 402 | 162 | 1 | 354 | 84 |
| 18 | 3775 | 3775 | 3.73 | 402 | 167 | 1 | 342 | 84 |
| 19 | 3748 | 3748 | 3.84 | 402 | 167 | 1 | 342 | 84 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "after-r1",
    "radius": 32,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "vertical": "fixed",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.708,
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
    "load_start": 4.17,
    "load_end": 3.34,
    "unix_time": 1791214726,
    "date_utc": "2026-10-05 15:38:46 UTC",
    "frames_presented": 2867
  },
  "frames": {
    "count": 73353,
    "seconds": 20,
    "avg_fps": 3667.6,
    "mean_ms": 0.2727,
    "p50_ms": 0.2432,
    "p95_ms": 0.5346,
    "p99_ms": 0.6895,
    "p999_ms": 2.7028,
    "min_ms": 0.0707,
    "max_ms": 4.2764,
    "low_1_fps": 809.3,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 1.6517,
      "p99_ms": 2.5677,
      "max_ms": 2.5677
    },
    "other": {
      "frames": 73333,
      "p50_ms": 0.2432,
      "p99_ms": 0.6884,
      "max_ms": 4.2764
    }
  },
  "world": {
    "generated": 8040,
    "generated_per_s": 402,
    "meshes": 3145,
    "meshes_per_s": 157.2,
    "removed": 2443,
    "removed_per_s": 122.1,
    "jobs": 13883,
    "jobs_per_s": 694.1,
    "upload_quads": 705712,
    "upload_quads_per_s": 35285.4,
    "crossings": 20,
    "queued": {
      "avg": 0.7,
      "p95": 0,
      "max": 354,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.3,
      "p95": 0,
      "max": 102,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 1.9,
      "p95": 0,
      "max": 719,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "cut_off": 0,
      "avg_ms": 8.8788,
      "max_ms": 10.8759
    },
    "catch_up_s": 0.001,
    "final": {
      "chunks": 21870,
      "data_ready": 21870,
      "mesh_ready": 8496,
      "mesh_empty": 12021,
      "gpu_chunks": 8496,
      "quads": 1904612,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 25918,
      "t_s": 7.196,
      "ms": 4.2764,
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
      "frame": 31037,
      "t_s": 8.615,
      "ms": 4.2692,
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
      "frame": 8660,
      "t_s": 2.392,
      "ms": 4.1572,
      "crossed": false,
      "presented": true,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 48751,
      "t_s": 13.441,
      "ms": 4.1499,
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
      "frame": 16621,
      "t_s": 4.613,
      "ms": 4.0353,
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
      "frames": 3617,
      "fps": 3616.8,
      "worst_ms": 3.5408,
      "generated": 402,
      "meshes": 159,
      "crossings": 1,
      "queued_max": 336,
      "in_flight_max": 66
    },
    {
      "second": 1,
      "frames": 3615,
      "fps": 3614.6,
      "worst_ms": 3.2197,
      "generated": 402,
      "meshes": 157,
      "crossings": 1,
      "queued_max": 342,
      "in_flight_max": 102
    },
    {
      "second": 2,
      "frames": 3591,
      "fps": 3591,
      "worst_ms": 4.1572,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 348,
      "in_flight_max": 90
    },
    {
      "second": 3,
      "frames": 3582,
      "fps": 3581.8,
      "worst_ms": 3.8359,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 272,
      "in_flight_max": 90
    },
    {
      "second": 4,
      "frames": 3585,
      "fps": 3585.1,
      "worst_ms": 4.0353,
      "generated": 402,
      "meshes": 156,
      "crossings": 1,
      "queued_max": 342,
      "in_flight_max": 78
    },
    {
      "second": 5,
      "frames": 3607,
      "fps": 3605.5,
      "worst_ms": 4.0055,
      "generated": 402,
      "meshes": 154,
      "crossings": 1,
      "queued_max": 298,
      "in_flight_max": 72
    },
    {
      "second": 6,
      "frames": 3614,
      "fps": 3616.1,
      "worst_ms": 3.4758,
      "generated": 402,
      "meshes": 157,
      "crossings": 1,
      "queued_max": 284,
      "in_flight_max": 78
    },
    {
      "second": 7,
      "frames": 3583,
      "fps": 3583.1,
      "worst_ms": 4.2764,
      "generated": 402,
      "meshes": 159,
      "crossings": 1,
      "queued_max": 336,
      "in_flight_max": 84
    },
    {
      "second": 8,
      "frames": 3635,
      "fps": 3633.6,
      "worst_ms": 4.2692,
      "generated": 402,
      "meshes": 150,
      "crossings": 1,
      "queued_max": 266,
      "in_flight_max": 96
    },
    {
      "second": 9,
      "frames": 3636,
      "fps": 3636.9,
      "worst_ms": 3.8759,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 335,
      "in_flight_max": 96
    },
    {
      "second": 10,
      "frames": 3654,
      "fps": 3654,
      "worst_ms": 3.8888,
      "generated": 402,
      "meshes": 149,
      "crossings": 1,
      "queued_max": 336,
      "in_flight_max": 84
    },
    {
      "second": 11,
      "frames": 3678,
      "fps": 3678.1,
      "worst_ms": 3.8678,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 270,
      "in_flight_max": 96
    },
    {
      "second": 12,
      "frames": 3698,
      "fps": 3698.4,
      "worst_ms": 3.5765,
      "generated": 402,
      "meshes": 152,
      "crossings": 1,
      "queued_max": 342,
      "in_flight_max": 90
    },
    {
      "second": 13,
      "frames": 3723,
      "fps": 3721.8,
      "worst_ms": 4.1499,
      "generated": 402,
      "meshes": 151,
      "crossings": 1,
      "queued_max": 336,
      "in_flight_max": 84
    },
    {
      "second": 14,
      "frames": 3737,
      "fps": 3737.9,
      "worst_ms": 3.5456,
      "generated": 402,
      "meshes": 158,
      "crossings": 1,
      "queued_max": 279,
      "in_flight_max": 90
    },
    {
      "second": 15,
      "frames": 3721,
      "fps": 3720.4,
      "worst_ms": 3.9013,
      "generated": 402,
      "meshes": 163,
      "crossings": 1,
      "queued_max": 348,
      "in_flight_max": 66
    },
    {
      "second": 16,
      "frames": 3758,
      "fps": 3758.3,
      "worst_ms": 3.7586,
      "generated": 402,
      "meshes": 166,
      "crossings": 1,
      "queued_max": 348,
      "in_flight_max": 60
    },
    {
      "second": 17,
      "frames": 3796,
      "fps": 3795.9,
      "worst_ms": 2.8821,
      "generated": 402,
      "meshes": 162,
      "crossings": 1,
      "queued_max": 354,
      "in_flight_max": 84
    },
    {
      "second": 18,
      "frames": 3775,
      "fps": 3775.4,
      "worst_ms": 3.7268,
      "generated": 402,
      "meshes": 167,
      "crossings": 1,
      "queued_max": 342,
      "in_flight_max": 84
    },
    {
      "second": 19,
      "frames": 3748,
      "fps": 3747.9,
      "worst_ms": 3.8442,
      "generated": 402,
      "meshes": 167,
      "crossings": 1,
      "queued_max": 342,
      "in_flight_max": 84
    }
  ]
}
```
