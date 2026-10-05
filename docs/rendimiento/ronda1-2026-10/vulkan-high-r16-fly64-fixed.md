# boxel stress: fly r16 64 vox/s

- Run: 2026-10-05 15:38:00 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 6.72 -> 5.32.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.47 s with 5598 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 156446 | 7822 | 0.10 | 0.23 | 0.55 | 2.28 | 4.58 | 951 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 0.74 | 1.84 | 1.84 |
| other | 156406 | 0.10 | 0.55 | 4.58 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 420 | 158 | 145 | 717 | 35895 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.3 | 0 | 192 | 0 | 0 |
| jobs in flight | 0.2 | 0 | 120 | 0 | 0 |
| chunks generating or meshing | 0.8 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 5 ms on average, 8 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 6102 chunks, 2200 meshes (3199 empty), 2200 on the GPU, 509198 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 20685 | 2.67 | 4.58 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 49295 | 6.32 | 4.33 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 147620 | 18.93 | 4.30 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 9671 | 1.25 | 4.24 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 57806 | 7.44 | 4.15 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 7711 | 7702 | 3.61 | 420 | 159 | 2 | 180 | 30 |
| 1 | 7674 | 7682 | 4.24 | 420 | 159 | 2 | 186 | 108 |
| 2 | 7864 | 7864 | 4.58 | 420 | 165 | 2 | 186 | 42 |
| 3 | 7854 | 7854 | 4.05 | 420 | 166 | 2 | 174 | 48 |
| 4 | 7906 | 7906 | 4.15 | 420 | 167 | 2 | 186 | 120 |
| 5 | 7830 | 7830 | 4.06 | 420 | 163 | 2 | 192 | 60 |
| 6 | 7642 | 7642 | 4.33 | 420 | 158 | 2 | 186 | 84 |
| 7 | 7539 | 7539 | 4.15 | 420 | 144 | 2 | 180 | 60 |
| 8 | 6982 | 6982 | 3.17 | 420 | 147 | 2 | 180 | 54 |
| 9 | 7306 | 7306 | 3.31 | 420 | 156 | 2 | 180 | 54 |
| 10 | 7614 | 7614 | 3.74 | 420 | 152 | 2 | 180 | 36 |
| 11 | 7711 | 7711 | 4.01 | 420 | 151 | 2 | 168 | 66 |
| 12 | 8135 | 8135 | 4.12 | 420 | 153 | 2 | 186 | 48 |
| 13 | 8176 | 8160 | 3.58 | 420 | 150 | 2 | 180 | 48 |
| 14 | 7983 | 7999 | 3.96 | 420 | 151 | 2 | 180 | 54 |
| 15 | 7812 | 7812 | 3.68 | 420 | 157 | 2 | 186 | 42 |
| 16 | 7945 | 7945 | 4.10 | 420 | 159 | 2 | 168 | 66 |
| 17 | 8206 | 8206 | 3.83 | 420 | 159 | 2 | 180 | 48 |
| 18 | 8294 | 8290 | 4.30 | 420 | 166 | 2 | 186 | 72 |
| 19 | 8262 | 8253 | 3.93 | 420 | 172 | 2 | 180 | 60 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "after-r1",
    "radius": 16,
    "speed": 64,
    "seed": "11566621",
    "trees": true,
    "vertical": "fixed",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.471,
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
    "load_start": 6.72,
    "load_end": 5.32,
    "unix_time": 1791214680,
    "date_utc": "2026-10-05 15:38:00 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 156446,
    "seconds": 20.002,
    "avg_fps": 7821.7,
    "mean_ms": 0.1279,
    "p50_ms": 0.1035,
    "p95_ms": 0.232,
    "p99_ms": 0.5501,
    "p999_ms": 2.2754,
    "min_ms": 0.0634,
    "max_ms": 4.5778,
    "low_1_fps": 950.6,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 0.7431,
      "p99_ms": 1.8356,
      "max_ms": 1.8356
    },
    "other": {
      "frames": 156406,
      "p50_ms": 0.1035,
      "p99_ms": 0.5469,
      "max_ms": 4.5778
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
    "jobs_per_s": 716.5,
    "upload_quads": 717955,
    "upload_quads_per_s": 35894.8,
    "crossings": 40,
    "queued": {
      "avg": 0.3,
      "p95": 0,
      "max": 192,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.2,
      "p95": 0,
      "max": 120,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 0.8,
      "p95": 0,
      "max": 374,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 40,
      "finished": 40,
      "cut_off": 0,
      "avg_ms": 4.7675,
      "max_ms": 8.0405
    },
    "catch_up_s": 0,
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
      "frame": 20685,
      "t_s": 2.67,
      "ms": 4.5778,
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
      "frame": 49295,
      "t_s": 6.317,
      "ms": 4.3289,
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
      "frame": 147620,
      "t_s": 18.928,
      "ms": 4.3042,
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
      "frame": 9671,
      "t_s": 1.249,
      "ms": 4.243,
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
      "frame": 57806,
      "t_s": 7.44,
      "ms": 4.1514,
      "crossed": false,
      "presented": true,
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
      "frames": 7711,
      "fps": 7702.1,
      "worst_ms": 3.6124,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 30
    },
    {
      "second": 1,
      "frames": 7674,
      "fps": 7682.5,
      "worst_ms": 4.243,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 108
    },
    {
      "second": 2,
      "frames": 7864,
      "fps": 7864.1,
      "worst_ms": 4.5778,
      "generated": 420,
      "meshes": 165,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 42
    },
    {
      "second": 3,
      "frames": 7854,
      "fps": 7854.2,
      "worst_ms": 4.0527,
      "generated": 420,
      "meshes": 166,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 48
    },
    {
      "second": 4,
      "frames": 7906,
      "fps": 7905.6,
      "worst_ms": 4.1468,
      "generated": 420,
      "meshes": 167,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 120
    },
    {
      "second": 5,
      "frames": 7830,
      "fps": 7829.8,
      "worst_ms": 4.0551,
      "generated": 420,
      "meshes": 163,
      "crossings": 2,
      "queued_max": 192,
      "in_flight_max": 60
    },
    {
      "second": 6,
      "frames": 7642,
      "fps": 7642,
      "worst_ms": 4.3289,
      "generated": 420,
      "meshes": 158,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 84
    },
    {
      "second": 7,
      "frames": 7539,
      "fps": 7538.7,
      "worst_ms": 4.1514,
      "generated": 420,
      "meshes": 144,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 60
    },
    {
      "second": 8,
      "frames": 6982,
      "fps": 6982.3,
      "worst_ms": 3.1741,
      "generated": 420,
      "meshes": 147,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 54
    },
    {
      "second": 9,
      "frames": 7306,
      "fps": 7306.3,
      "worst_ms": 3.3149,
      "generated": 420,
      "meshes": 156,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 54
    },
    {
      "second": 10,
      "frames": 7614,
      "fps": 7613.5,
      "worst_ms": 3.7433,
      "generated": 420,
      "meshes": 152,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 36
    },
    {
      "second": 11,
      "frames": 7711,
      "fps": 7711,
      "worst_ms": 4.0126,
      "generated": 420,
      "meshes": 151,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 66
    },
    {
      "second": 12,
      "frames": 8135,
      "fps": 8135.5,
      "worst_ms": 4.1172,
      "generated": 420,
      "meshes": 153,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 48
    },
    {
      "second": 13,
      "frames": 8176,
      "fps": 8159.5,
      "worst_ms": 3.5816,
      "generated": 420,
      "meshes": 150,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 48
    },
    {
      "second": 14,
      "frames": 7983,
      "fps": 7999.2,
      "worst_ms": 3.9638,
      "generated": 420,
      "meshes": 151,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 54
    },
    {
      "second": 15,
      "frames": 7812,
      "fps": 7812.2,
      "worst_ms": 3.6802,
      "generated": 420,
      "meshes": 157,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 42
    },
    {
      "second": 16,
      "frames": 7945,
      "fps": 7944.9,
      "worst_ms": 4.1031,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 66
    },
    {
      "second": 17,
      "frames": 8206,
      "fps": 8206.2,
      "worst_ms": 3.8332,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 48
    },
    {
      "second": 18,
      "frames": 8294,
      "fps": 8289.6,
      "worst_ms": 4.3042,
      "generated": 420,
      "meshes": 166,
      "crossings": 2,
      "queued_max": 186,
      "in_flight_max": 72
    },
    {
      "second": 19,
      "frames": 8262,
      "fps": 8252.9,
      "worst_ms": 3.9327,
      "generated": 420,
      "meshes": 172,
      "crossings": 2,
      "queued_max": 180,
      "in_flight_max": 60
    }
  ]
}
```
