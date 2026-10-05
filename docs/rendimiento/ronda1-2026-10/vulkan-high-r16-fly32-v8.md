# boxel stress: fly r16 32 vox/s

- Run: 2026-10-05 15:37:38 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 8.77 -> 7.13.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.42 s with 17727 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 160348 | 8017 | 0.10 | 0.21 | 0.50 | 2.16 | 4.11 | 996 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 20 | 1.47 | 2.39 | 2.39 |
| other | 160328 | 0.10 | 0.50 | 4.11 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 210 | 79 | 63 | 360 | 18092 | 20 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.1 | 0 | 174 | 0 | 0 |
| jobs in flight | 0.1 | 0 | 96 | 0 | 0 |
| chunks generating or meshing | 0.3 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 5 ms on average, 7 ms at most (20 of 20 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 19323 chunks, 2270 meshes (12787 empty), 2270 on the GPU, 508896 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 104232 | 12.93 | 4.11 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 96396 | 11.96 | 4.09 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 124781 | 15.55 | 4.08 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 107167 | 13.31 | 4.00 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 34634 | 4.37 | 3.97 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 7767 | 7766 | 3.15 | 210 | 79 | 1 | 162 | 48 |
| 1 | 7808 | 7808 | 3.22 | 210 | 80 | 1 | 107 | 72 |
| 2 | 8010 | 8010 | 2.95 | 210 | 83 | 1 | 168 | 66 |
| 3 | 7986 | 7985 | 3.15 | 210 | 76 | 1 | 168 | 54 |
| 4 | 8002 | 8003 | 3.97 | 210 | 81 | 1 | 156 | 78 |
| 5 | 7979 | 7979 | 3.69 | 210 | 84 | 1 | 168 | 48 |
| 6 | 8206 | 8206 | 3.22 | 210 | 83 | 1 | 162 | 54 |
| 7 | 8305 | 8305 | 3.97 | 210 | 83 | 1 | 168 | 66 |
| 8 | 8449 | 8448 | 3.19 | 210 | 83 | 1 | 168 | 54 |
| 9 | 8062 | 8063 | 3.90 | 210 | 84 | 1 | 88 | 96 |
| 10 | 8120 | 8120 | 3.88 | 210 | 84 | 1 | 174 | 48 |
| 11 | 8027 | 8026 | 4.09 | 210 | 79 | 1 | 106 | 72 |
| 12 | 8002 | 8000 | 4.11 | 210 | 80 | 1 | 156 | 78 |
| 13 | 7924 | 7924 | 4.00 | 210 | 78 | 1 | 162 | 60 |
| 14 | 7810 | 7812 | 3.89 | 210 | 73 | 1 | 115 | 66 |
| 15 | 7841 | 7841 | 4.08 | 210 | 71 | 1 | 162 | 54 |
| 16 | 8055 | 8055 | 3.55 | 210 | 75 | 1 | 150 | 84 |
| 17 | 7971 | 7971 | 3.63 | 210 | 72 | 1 | 168 | 84 |
| 18 | 7999 | 7999 | 3.12 | 210 | 77 | 1 | 162 | 48 |
| 19 | 8025 | 8024 | 3.96 | 210 | 79 | 1 | 118 | 78 |

## Data

For `tools/bench_report.py` (format 1).

```json
{
  "format": 1,
  "run": {
    "scenario": "fly",
    "label": "after-r1",
    "radius": 16,
    "speed": 32,
    "seed": "11566621",
    "trees": true,
    "vertical": "8",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.416,
    "settle_chunks": 17727,
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
    "load_start": 8.77,
    "load_end": 7.13,
    "unix_time": 1791214658,
    "date_utc": "2026-10-05 15:37:38 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 160348,
    "seconds": 20,
    "avg_fps": 8017.4,
    "mean_ms": 0.1247,
    "p50_ms": 0.1033,
    "p95_ms": 0.2131,
    "p99_ms": 0.5029,
    "p999_ms": 2.1609,
    "min_ms": 0.064,
    "max_ms": 4.1059,
    "low_1_fps": 995.8,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 20,
      "p50_ms": 1.4714,
      "p99_ms": 2.3888,
      "max_ms": 2.3888
    },
    "other": {
      "frames": 160328,
      "p50_ms": 0.1033,
      "p99_ms": 0.5013,
      "max_ms": 4.1059
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
      "avg": 0.1,
      "p95": 0,
      "max": 174,
      "first": 0,
      "last": 0
    },
    "in_flight": {
      "avg": 0.1,
      "p95": 0,
      "max": 96,
      "first": 0,
      "last": 0
    },
    "pending_chunks": {
      "avg": 0.3,
      "p95": 0,
      "max": 374,
      "first": 0,
      "last": 0
    },
    "backlog_trend_per_s": 0,
    "border_catch_up": {
      "crossings": 20,
      "finished": 20,
      "cut_off": 0,
      "avg_ms": 5.3135,
      "max_ms": 6.701
    },
    "catch_up_s": 0,
    "final": {
      "chunks": 19323,
      "data_ready": 19323,
      "mesh_ready": 2270,
      "mesh_empty": 12787,
      "gpu_chunks": 2270,
      "quads": 508896,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 104232,
      "t_s": 12.932,
      "ms": 4.1059,
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
      "frame": 96396,
      "t_s": 11.957,
      "ms": 4.0864,
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
      "frame": 124781,
      "t_s": 15.551,
      "ms": 4.0825,
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
      "frame": 107167,
      "t_s": 13.307,
      "ms": 4.0032,
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
      "frame": 34634,
      "t_s": 4.374,
      "ms": 3.9694,
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
      "frames": 7767,
      "fps": 7766.1,
      "worst_ms": 3.1491,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 162,
      "in_flight_max": 48
    },
    {
      "second": 1,
      "frames": 7808,
      "fps": 7808.5,
      "worst_ms": 3.2243,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 107,
      "in_flight_max": 72
    },
    {
      "second": 2,
      "frames": 8010,
      "fps": 8009.7,
      "worst_ms": 2.9525,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 168,
      "in_flight_max": 66
    },
    {
      "second": 3,
      "frames": 7986,
      "fps": 7984.8,
      "worst_ms": 3.1533,
      "generated": 210,
      "meshes": 76,
      "crossings": 1,
      "queued_max": 168,
      "in_flight_max": 54
    },
    {
      "second": 4,
      "frames": 8002,
      "fps": 8003.3,
      "worst_ms": 3.9694,
      "generated": 210,
      "meshes": 81,
      "crossings": 1,
      "queued_max": 156,
      "in_flight_max": 78
    },
    {
      "second": 5,
      "frames": 7979,
      "fps": 7979.1,
      "worst_ms": 3.6943,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 168,
      "in_flight_max": 48
    },
    {
      "second": 6,
      "frames": 8206,
      "fps": 8206.4,
      "worst_ms": 3.2234,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 162,
      "in_flight_max": 54
    },
    {
      "second": 7,
      "frames": 8305,
      "fps": 8305,
      "worst_ms": 3.9679,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 168,
      "in_flight_max": 66
    },
    {
      "second": 8,
      "frames": 8449,
      "fps": 8448.3,
      "worst_ms": 3.1907,
      "generated": 210,
      "meshes": 83,
      "crossings": 1,
      "queued_max": 168,
      "in_flight_max": 54
    },
    {
      "second": 9,
      "frames": 8062,
      "fps": 8062.6,
      "worst_ms": 3.9034,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 88,
      "in_flight_max": 96
    },
    {
      "second": 10,
      "frames": 8120,
      "fps": 8120.2,
      "worst_ms": 3.8825,
      "generated": 210,
      "meshes": 84,
      "crossings": 1,
      "queued_max": 174,
      "in_flight_max": 48
    },
    {
      "second": 11,
      "frames": 8027,
      "fps": 8026.2,
      "worst_ms": 4.0864,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 106,
      "in_flight_max": 72
    },
    {
      "second": 12,
      "frames": 8002,
      "fps": 7999.9,
      "worst_ms": 4.1059,
      "generated": 210,
      "meshes": 80,
      "crossings": 1,
      "queued_max": 156,
      "in_flight_max": 78
    },
    {
      "second": 13,
      "frames": 7924,
      "fps": 7924.2,
      "worst_ms": 4.0032,
      "generated": 210,
      "meshes": 78,
      "crossings": 1,
      "queued_max": 162,
      "in_flight_max": 60
    },
    {
      "second": 14,
      "frames": 7810,
      "fps": 7812.1,
      "worst_ms": 3.8939,
      "generated": 210,
      "meshes": 73,
      "crossings": 1,
      "queued_max": 115,
      "in_flight_max": 66
    },
    {
      "second": 15,
      "frames": 7841,
      "fps": 7841.1,
      "worst_ms": 4.0825,
      "generated": 210,
      "meshes": 71,
      "crossings": 1,
      "queued_max": 162,
      "in_flight_max": 54
    },
    {
      "second": 16,
      "frames": 8055,
      "fps": 8055,
      "worst_ms": 3.5459,
      "generated": 210,
      "meshes": 75,
      "crossings": 1,
      "queued_max": 150,
      "in_flight_max": 84
    },
    {
      "second": 17,
      "frames": 7971,
      "fps": 7971.1,
      "worst_ms": 3.6315,
      "generated": 210,
      "meshes": 72,
      "crossings": 1,
      "queued_max": 168,
      "in_flight_max": 84
    },
    {
      "second": 18,
      "frames": 7999,
      "fps": 7999.2,
      "worst_ms": 3.1189,
      "generated": 210,
      "meshes": 77,
      "crossings": 1,
      "queued_max": 162,
      "in_flight_max": 48
    },
    {
      "second": 19,
      "frames": 8025,
      "fps": 8024.5,
      "worst_ms": 3.9572,
      "generated": 210,
      "meshes": 79,
      "crossings": 1,
      "queued_max": 118,
      "in_flight_max": 78
    }
  ]
}
```
