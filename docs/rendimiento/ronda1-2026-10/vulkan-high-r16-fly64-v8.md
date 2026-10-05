# boxel stress: fly r16 64 vox/s

- Run: 2026-10-05 15:38:23 UTC, release build, label `after-r1`, AMD Ryzen 7 250 w/ Radeon 780M Graphics (16 threads), load 5.32 -> 4.10.
- GPU: NVIDIA GeForce RTX 5050 Laptop GPU, Vulkan High (Discrete), driver `NVIDIA 595.91.07`, present immediate, window 1280x720.
- World: seed 11566621, trees on, radius 16, settled in 0.42 s with 17727 chunks; 20 s measured after 2 s of warmup.

## Frames

| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | > 6.94 ms | > 16.7 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 158708 | 7935 | 0.10 | 0.22 | 0.53 | 2.28 | 4.30 | 942 | 0 | 0 |

| frames | count | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|
| crossed a chunk border | 40 | 1.48 | 2.27 | 2.27 |
| other | 158668 | 0.10 | 0.53 | 4.30 |

## World

| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |
|---:|---:|---:|---:|---:|---:|
| 420 | 158 | 145 | 717 | 35898 | 40 |

| backlog | avg | p95 | max | first | last |
|---|---:|---:|---:|---:|---:|
| jobs queued | 0.2 | 0 | 174 | 0 | 0 |
| jobs in flight | 0.1 | 0 | 96 | 0 | 0 |
| chunks generating or meshing | 0.6 | 0 | 374 | 0 | 0 |

After a chunk border the world finished the new chunks in 5 ms on average, 9 ms at most (40 of 40 borders finished before the next one). Queue trend +0.0 jobs/s (slope over the measurement; clearly above 0: the world falls behind). It settled again 0.00 s after the camera stopped. At the end: 19323 chunks, 2200 meshes (12857 empty), 2200 on the GPU, 509198 quads, 2 draws.

## Worst 5 frames

| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | queued | in flight |
|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|
| 77179 | 9.80 | 4.30 | no | yes | 0 | 0 | 0 | 0 | 0 | 0 |
| 116083 | 14.64 | 4.27 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 111671 | 14.09 | 4.22 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 66627 | 8.44 | 4.14 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |
| 5445 | 0.71 | 4.10 | no | no | 0 | 0 | 0 | 0 | 0 | 0 |

## Per second

| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight max |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 7704 | 7704 | 4.10 | 420 | 159 | 2 | 174 | 72 |
| 1 | 7743 | 7743 | 4.09 | 420 | 159 | 2 | 174 | 78 |
| 2 | 7854 | 7855 | 3.94 | 420 | 165 | 2 | 168 | 60 |
| 3 | 8001 | 8001 | 3.90 | 420 | 166 | 2 | 174 | 66 |
| 4 | 7998 | 7998 | 3.97 | 420 | 167 | 2 | 174 | 48 |
| 5 | 8067 | 8067 | 3.48 | 420 | 163 | 2 | 156 | 60 |
| 6 | 7909 | 7909 | 3.11 | 420 | 158 | 2 | 168 | 78 |
| 7 | 7811 | 7810 | 3.46 | 420 | 144 | 2 | 168 | 60 |
| 8 | 7873 | 7874 | 4.14 | 420 | 147 | 2 | 168 | 78 |
| 9 | 7811 | 7811 | 4.30 | 420 | 156 | 2 | 168 | 54 |
| 10 | 7877 | 7877 | 4.01 | 420 | 152 | 2 | 138 | 96 |
| 11 | 8059 | 8059 | 3.92 | 420 | 151 | 2 | 168 | 60 |
| 12 | 8048 | 8047 | 3.56 | 420 | 153 | 2 | 174 | 60 |
| 13 | 8144 | 8145 | 3.57 | 420 | 150 | 2 | 168 | 73 |
| 14 | 8075 | 8075 | 4.27 | 420 | 151 | 2 | 174 | 54 |
| 15 | 7903 | 7903 | 3.74 | 420 | 157 | 2 | 156 | 66 |
| 16 | 7915 | 7915 | 3.41 | 420 | 159 | 2 | 174 | 66 |
| 17 | 7995 | 7991 | 3.01 | 420 | 159 | 2 | 168 | 54 |
| 18 | 7974 | 7978 | 3.65 | 420 | 166 | 2 | 168 | 72 |
| 19 | 7947 | 7947 | 4.02 | 420 | 172 | 2 | 174 | 90 |

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
    "vertical": "8",
    "duration_s": 20,
    "warmup_s": 2,
    "settle_s": 0.424,
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
    "load_start": 5.32,
    "load_end": 4.1,
    "unix_time": 1791214703,
    "date_utc": "2026-10-05 15:38:23 UTC",
    "frames_presented": 2868
  },
  "frames": {
    "count": 158708,
    "seconds": 20,
    "avg_fps": 7935.4,
    "mean_ms": 0.126,
    "p50_ms": 0.103,
    "p95_ms": 0.2213,
    "p99_ms": 0.5325,
    "p999_ms": 2.2844,
    "min_ms": 0.0638,
    "max_ms": 4.2959,
    "low_1_fps": 941.8,
    "over_144hz": 0,
    "over_60hz": 0,
    "crossing": {
      "frames": 40,
      "p50_ms": 1.4839,
      "p99_ms": 2.2662,
      "max_ms": 2.2662
    },
    "other": {
      "frames": 158668,
      "p50_ms": 0.103,
      "p99_ms": 0.5281,
      "max_ms": 4.2959
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
      "avg": 0.2,
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
      "avg": 0.6,
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
      "avg_ms": 5.4175,
      "max_ms": 8.5676
    },
    "catch_up_s": 0,
    "final": {
      "chunks": 19323,
      "data_ready": 19323,
      "mesh_ready": 2200,
      "mesh_empty": 12857,
      "gpu_chunks": 2200,
      "quads": 509198,
      "draws": 2
    }
  },
  "worst": [
    {
      "frame": 77179,
      "t_s": 9.797,
      "ms": 4.2959,
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
      "frame": 116083,
      "t_s": 14.641,
      "ms": 4.2723,
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
      "frame": 111671,
      "t_s": 14.092,
      "ms": 4.2245,
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
      "frame": 66627,
      "t_s": 8.444,
      "ms": 4.1379,
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
      "frame": 5445,
      "t_s": 0.708,
      "ms": 4.1007,
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
      "frames": 7704,
      "fps": 7703.6,
      "worst_ms": 4.1007,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 72
    },
    {
      "second": 1,
      "frames": 7743,
      "fps": 7742.6,
      "worst_ms": 4.0911,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 78
    },
    {
      "second": 2,
      "frames": 7854,
      "fps": 7854.5,
      "worst_ms": 3.9426,
      "generated": 420,
      "meshes": 165,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 60
    },
    {
      "second": 3,
      "frames": 8001,
      "fps": 8000.9,
      "worst_ms": 3.8958,
      "generated": 420,
      "meshes": 166,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 66
    },
    {
      "second": 4,
      "frames": 7998,
      "fps": 7997.7,
      "worst_ms": 3.9733,
      "generated": 420,
      "meshes": 167,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 48
    },
    {
      "second": 5,
      "frames": 8067,
      "fps": 8067,
      "worst_ms": 3.4826,
      "generated": 420,
      "meshes": 163,
      "crossings": 2,
      "queued_max": 156,
      "in_flight_max": 60
    },
    {
      "second": 6,
      "frames": 7909,
      "fps": 7909.3,
      "worst_ms": 3.1137,
      "generated": 420,
      "meshes": 158,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 78
    },
    {
      "second": 7,
      "frames": 7811,
      "fps": 7809.8,
      "worst_ms": 3.4639,
      "generated": 420,
      "meshes": 144,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 60
    },
    {
      "second": 8,
      "frames": 7873,
      "fps": 7874,
      "worst_ms": 4.1379,
      "generated": 420,
      "meshes": 147,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 78
    },
    {
      "second": 9,
      "frames": 7811,
      "fps": 7811.5,
      "worst_ms": 4.2959,
      "generated": 420,
      "meshes": 156,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 54
    },
    {
      "second": 10,
      "frames": 7877,
      "fps": 7876.7,
      "worst_ms": 4.0107,
      "generated": 420,
      "meshes": 152,
      "crossings": 2,
      "queued_max": 138,
      "in_flight_max": 96
    },
    {
      "second": 11,
      "frames": 8059,
      "fps": 8059.1,
      "worst_ms": 3.9202,
      "generated": 420,
      "meshes": 151,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 60
    },
    {
      "second": 12,
      "frames": 8048,
      "fps": 8047,
      "worst_ms": 3.5639,
      "generated": 420,
      "meshes": 153,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 60
    },
    {
      "second": 13,
      "frames": 8144,
      "fps": 8145,
      "worst_ms": 3.5719,
      "generated": 420,
      "meshes": 150,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 73
    },
    {
      "second": 14,
      "frames": 8075,
      "fps": 8074.7,
      "worst_ms": 4.2723,
      "generated": 420,
      "meshes": 151,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 54
    },
    {
      "second": 15,
      "frames": 7903,
      "fps": 7902.8,
      "worst_ms": 3.7368,
      "generated": 420,
      "meshes": 157,
      "crossings": 2,
      "queued_max": 156,
      "in_flight_max": 66
    },
    {
      "second": 16,
      "frames": 7915,
      "fps": 7915.3,
      "worst_ms": 3.4068,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 66
    },
    {
      "second": 17,
      "frames": 7995,
      "fps": 7991,
      "worst_ms": 3.0147,
      "generated": 420,
      "meshes": 159,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 54
    },
    {
      "second": 18,
      "frames": 7974,
      "fps": 7978.1,
      "worst_ms": 3.6459,
      "generated": 420,
      "meshes": 166,
      "crossings": 2,
      "queued_max": 168,
      "in_flight_max": 72
    },
    {
      "second": 19,
      "frames": 7947,
      "fps": 7947,
      "worst_ms": 4.0173,
      "generated": 420,
      "meshes": 172,
      "crossings": 2,
      "queued_max": 174,
      "in_flight_max": 90
    }
  ]
}
```
