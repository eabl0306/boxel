# boxel stress: spin r32

- Run: 2026-10-05 14:03:23 UTC, release build, label `e933486`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 2.35 -> 2.00.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 32, settled in 1.70 s with 20838 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 72818 | 3641 | 0.25 | 0.50 | 0.69 | 2.75 | 4.28 | 825 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 0 | - | - | - |
| other | 72818 | 0.25 | 0.69 | 4.28 |

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
| 31446 | 8.56 | 4.28 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 57118 | 15.69 | 4.22 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 40048 | 10.94 | 4.16 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 11709 | 3.21 | 4.14 | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 44787 | 12.26 | 4.12 | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 3610 | 3610 | 3.81 | 0 | 0 | 0 | 0 | 0 |
| 1 | 3647 | 3647 | 3.96 | 0 | 0 | 0 | 0 | 0 |
| 2 | 3685 | 3685 | 3.99 | 0 | 0 | 0 | 0 | 0 |
| 3 | 3661 | 3661 | 4.14 | 0 | 0 | 0 | 0 | 0 |
| 4 | 3747 | 3746 | 3.90 | 0 | 0 | 0 | 0 | 0 |
| 5 | 3762 | 3763 | 3.91 | 0 | 0 | 0 | 0 | 0 |
| 6 | 3638 | 3638 | 3.77 | 0 | 0 | 0 | 0 | 0 |
| 7 | 3661 | 3661 | 4.08 | 0 | 0 | 0 | 0 | 0 |
| 8 | 3614 | 3614 | 4.28 | 0 | 0 | 0 | 0 | 0 |
| 9 | 3623 | 3623 | 4.09 | 0 | 0 | 0 | 0 | 0 |
| 10 | 3604 | 3605 | 4.16 | 0 | 0 | 0 | 0 | 0 |
| 11 | 3592 | 3592 | 3.41 | 0 | 0 | 0 | 0 | 0 |
| 12 | 3596 | 3595 | 4.12 | 0 | 0 | 0 | 0 | 0 |
| 13 | 3488 | 3488 | 3.91 | 0 | 0 | 0 | 0 | 0 |
| 14 | 3638 | 3638 | 3.11 | 0 | 0 | 0 | 0 | 0 |
| 15 | 3656 | 3656 | 4.22 | 0 | 0 | 0 | 0 | 0 |
| 16 | 3658 | 3658 | 3.98 | 0 | 0 | 0 | 0 | 0 |
| 17 | 3657 | 3658 | 3.59 | 0 | 0 | 0 | 0 | 0 |
| 18 | 3622 | 3622 | 3.98 | 0 | 0 | 0 | 0 | 0 |
| 19 | 3659 | 3659 | 3.74 | 0 | 0 | 0 | 0 | 0 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "spin",
    "label": "e933486",
    "radius": 32,
    "speed": 0,
    "seed": "11566621",
    "trees": true,
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 1.697,
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
    "load_start": 2.35,
    "load_end": 2,
    "unix_time": 1791209003,
    "date_utc": "2026-10-05 14:03:23 UTC",
    "frames_presented": 2870
  },
  "frames": {
    "count": 72818,
    "seconds": 20,
    "avg_fps": 3640.9,
    "mean_ms": 0.2747,
    "p50_ms": 0.2468,
    "p95_ms": 0.4982,
    "p99_ms": 0.685,
    "p999_ms": 2.7534,
    "min_ms": 0.0942,
    "max_ms": 4.2756,
    "low_1_fps": 825.1,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": null,
    "other": {
      "frames": 72818,
      "p50_ms": 0.2468,
      "p99_ms": 0.685,
      "max_ms": 4.2756
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
      "frame": 31446,
      "t_s": 8.563,
      "ms": 4.2756,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 57118,
      "t_s": 15.694,
      "ms": 4.2198,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 40048,
      "t_s": 10.938,
      "ms": 4.1594,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 11709,
      "t_s": 3.206,
      "ms": 4.1378,
      "crossed": false,
      "generated": 0,
      "meshes": 0,
      "removed": 0,
      "upload_quads": 0,
      "queued": 0,
      "in_flight": 0
    },
    {
      "frame": 44787,
      "t_s": 12.259,
      "ms": 4.1175,
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
      "frames": 3610,
      "fps": 3609.7,
      "worst_ms": 3.8122,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 1,
      "frames": 3647,
      "fps": 3647.3,
      "worst_ms": 3.9592,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 2,
      "frames": 3685,
      "fps": 3684.9,
      "worst_ms": 3.9859,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 3,
      "frames": 3661,
      "fps": 3660.7,
      "worst_ms": 4.1378,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 4,
      "frames": 3747,
      "fps": 3746.1,
      "worst_ms": 3.8966,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 5,
      "frames": 3762,
      "fps": 3762.5,
      "worst_ms": 3.9062,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 6,
      "frames": 3638,
      "fps": 3638.3,
      "worst_ms": 3.7715,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 7,
      "frames": 3661,
      "fps": 3660.8,
      "worst_ms": 4.0821,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 8,
      "frames": 3614,
      "fps": 3613.8,
      "worst_ms": 4.2756,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 9,
      "frames": 3623,
      "fps": 3623.1,
      "worst_ms": 4.0934,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 10,
      "frames": 3604,
      "fps": 3605,
      "worst_ms": 4.1594,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 11,
      "frames": 3592,
      "fps": 3591.7,
      "worst_ms": 3.4092,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 12,
      "frames": 3596,
      "fps": 3595.3,
      "worst_ms": 4.1175,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 13,
      "frames": 3488,
      "fps": 3488.3,
      "worst_ms": 3.913,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 14,
      "frames": 3638,
      "fps": 3638.2,
      "worst_ms": 3.1063,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 15,
      "frames": 3656,
      "fps": 3655.7,
      "worst_ms": 4.2198,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 16,
      "frames": 3658,
      "fps": 3657.7,
      "worst_ms": 3.9755,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 17,
      "frames": 3657,
      "fps": 3657.8,
      "worst_ms": 3.5904,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 18,
      "frames": 3622,
      "fps": 3621.6,
      "worst_ms": 3.9787,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    },
    {
      "second": 19,
      "frames": 3659,
      "fps": 3658.7,
      "worst_ms": 3.7355,
      "generated": 0,
      "meshes": 0,
      "crossings": 0,
      "queued_max": 0,
      "in_flight_max": 0
    }
  ]
}
```
