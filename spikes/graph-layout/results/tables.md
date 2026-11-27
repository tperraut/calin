
### 50 nodes / 80 edges: frame time (median / p95 ms, % of frames over 33 ms)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow |
| --- | --- | --- | --- | --- | --- | --- |
| svg-solid | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| svg-dom | 1x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| svg-best | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% |
| canvas-lod | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% |
| canvas-lod-dpr2 | 1x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| svg-solid | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |
| svg-dom | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| svg-best | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| canvas-lod | 4x | default | 16.7 / 16.8, 3.5% | 16.7 / 16.8, 0.5% | 16.7 / 33.3, 6.7% | 16.7 / 50, 21% |
| canvas-lod-dpr2 | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |

### 50 nodes / 80 edges: main-thread time per frame (ms: total task, of which script / style / layout)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow | DOM elements | JS heap MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| svg-solid | 1x | default | 1.1 (0.2 / 0.1 / 0.1) | 1.3 (0.2 / 0.1 / 0.1) | 1.4 (0.3 / 0.1 / 0.1) | 1.2 (0.3 / 0.1 / 0.1) | 311 | 5.8 |
| svg-dom | 1x | default | 0.9 (0.1 / 0 / 0) | 1 (0.1 / 0 / 0) | 1.2 (0.2 / 0.1 / 0.1) | 1.1 (0.2 / 0.1 / 0.1) | 311 | 5.7 |
| svg-best | 1x | default | 4.3 (0.1 / 0.1 / 0.1) | 5.1 (0.1 / 0.1 / 0.1) | 4.4 (0.2 / 0 / 0) | 3.1 (0.3 / 0.1 / 0) | 311 | 5.5 |
| canvas-lod | 1x | default | 6.7 (0.3 / 0 / 0) | 8.3 (0.2 / 0 / 0) | 9.5 (0.3 / 0 / 0) | 5.8 (0.2 / 0 / 0) | 19 | 4.2 |
| canvas-lod-dpr2 | 1x | default | 4.8 (0.3 / 0 / 0) | 5.4 (0.2 / 0 / 0) | 6.9 (0.3 / 0 / 0) | 5 (0.2 / 0 / 0) | 19 | 4.1 |
| svg-solid | 4x | default | 2.3 (0.4 / 0.2 / 0.1) | 2.3 (0.4 / 0.2 / 0.1) | 2.8 (0.6 / 0.2 / 0.1) | 2.5 (0.6 / 0.2 / 0.2) | 311 | 5.8 |
| svg-dom | 4x | default | 1.4 (0.1 / 0.1 / 0.1) | 1.4 (0.1 / 0.1 / 0.1) | 1.6 (0.2 / 0.1 / 0.1) | 1.5 (0.3 / 0.1 / 0.1) | 311 | 5.7 |
| svg-best | 4x | default | 8.4 (0.1 / 0.1 / 0.1) | 8.2 (0.1 / 0.1 / 0.1) | 7.9 (0.2 / 0 / 0) | 5.1 (0.4 / 0.1 / 0.1) | 311 | 6 |
| canvas-lod | 4x | default | 15.1 (0.5 / 0 / 0) | 14.8 (0.3 / 0 / 0) | 18.9 (0.4 / 0 / 0) | 17.2 (0.5 / 0 / 0) | 19 | 4.2 |
| canvas-lod-dpr2 | 4x | default | 9.5 (0.4 / 0 / 0) | 9.7 (0.3 / 0 / 0) | 12.5 (0.4 / 0 / 0) | 8 (0.2 / 0 / 0) | 19 | 4.2 |

Layouts during "grow" (50 nodes): svg-solid 1x: 10 layouts, median 41.5 ms; svg-dom 1x: 10 layouts, median 27.2 ms; svg-best 1x: 10 layouts, median 43.1 ms; canvas-lod 1x: 10 layouts, median 25.3 ms

Cold start to first layout applied (worker spawn + ELK compile + layout), ms: svg-solid 1x: 214; svg-dom 1x: 207; svg-best 1x: 182; canvas-lod 1x: 173; canvas-lod-dpr2 1x: 175; svg-solid 4x: 246; svg-dom 4x: 195; svg-best 4x: 188; canvas-lod 4x: 220; canvas-lod-dpr2 4x: 205 (layout: tree-order-noforce)

### 200 nodes / 300 edges: frame time (median / p95 ms, % of frames over 33 ms)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow |
| --- | --- | --- | --- | --- | --- | --- |
| svg-solid | 1x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| svg-dom | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| svg-best | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% |
| canvas-lod | 1x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| canvas-lod-dpr2 | 1x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |
| svg-solid | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 2.8% |
| svg-dom | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0.9% |
| svg-dom-cull | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |
| svg-dom-lod | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |
| svg-dom-pancss | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| svg-dom-pulsecss | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% |
| svg-dom-pulsecanvas | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0.3% |
| svg-best | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |
| canvas | 4x | default | 16.7 / 33.4, 22.4% | 16.7 / 33.4, 38.7% | 33.3 / 33.4, 75.6% | 16.7 / 33.4, 40.9% |
| canvas-lod | 4x | default | 16.7 / 33.3, 14.9% | 16.7 / 33.5, 43.3% | 33.3 / 33.4, 85.7% | 16.7 / 33.4, 44.6% |
| canvas-lod-2layer | 4x | default | 33.3 / 33.4, 52.6% | 33.3 / 50, 66.4% | 16.7 / 16.7, 0% | 16.8 / 33.4, 49.1% |
| canvas-lod-dpr2 | 4x | default | 16.7 / 16.7, 0% | 16.7 / 33.3, 6.1% | 16.7 / 33.4, 9.4% | 16.7 / 16.8, 3.5% |
| canvas-lod-dpr1 | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| solid-flow | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 1.5% |
| solid-flow-visible-only | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 4.8% |
| solid-flow | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 66.6, 23.3% |
| solid-flow-visible-only | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 66.6, 18.3% |
| svg-dom | 4x | --disable-gpu | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| canvas-lod | 4x | --disable-gpu | 16.7 / 33.4, 14.9% | 16.7 / 33.4, 32.1% | 33.3 / 33.4, 61.2% | 16.7 / 33.4, 37.3% |

### 200 nodes / 300 edges: main-thread time per frame (ms: total task, of which script / style / layout)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow | DOM elements | JS heap MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| svg-solid | 1x | default | 1.8 (0.3 / 0.1 / 0.1) | 1.9 (0.2 / 0.1 / 0.1) | 1.8 (0.3 / 0.1 / 0) | 3.4 (1 / 0.3 / 0.3) | 1131 | 4.7 |
| svg-dom | 1x | default | 1.6 (0.1 / 0.1 / 0.1) | 1.7 (0.1 / 0.1 / 0.1) | 1.8 (0.2 / 0.1 / 0) | 3.2 (0.8 / 0.3 / 0.3) | 1131 | 4.4 |
| svg-best | 1x | default | 4.8 (0.1 / 0.1 / 0.1) | 4.8 (0.2 / 0.1 / 0.1) | 4.9 (0.2 / 0 / 0) | 4.2 (0.9 / 0.4 / 0.1) | 1131 | 4.5 |
| canvas-lod | 1x | default | 8.5 (0.4 / 0 / 0) | 9.2 (0.3 / 0 / 0) | 10.1 (0.3 / 0 / 0) | 6.4 (0.2 / 0 / 0) | 19 | 7.2 |
| canvas-lod-dpr2 | 1x | default | 5.8 (0.4 / 0 / 0) | 7.1 (0.3 / 0 / 0) | 7.8 (0.3 / 0 / 0) | 5.1 (0.3 / 0 / 0) | 19 | 7.9 |
| svg-solid | 4x | default | 2.4 (0.3 / 0.2 / 0.1) | 2.8 (0.3 / 0.2 / 0.1) | 3.3 (0.5 / 0.2 / 0.1) | 7.4 (2.1 / 0.5 / 0.6) | 1131 | 7.3 |
| svg-dom | 4x | default | 2.1 (0.1 / 0.1 / 0.1) | 2.4 (0.1 / 0.1 / 0.1) | 2.6 (0.2 / 0.1 / 0.1) | 6.1 (1.4 / 0.5 / 0.6) | 1131 | 6.7 |
| svg-dom-cull | 4x | default | 4.7 (0.3 / 0.3 / 0.3) | 2.7 (0.2 / 0.2 / 0.2) | 2.6 (0.2 / 0.2 / 0.1) | 5.9 (1.4 / 0.6 / 0.7) | 1131 | 4.2 |
| svg-dom-lod | 4x | default | 2.1 (0.1 / 0.1 / 0.1) | 2.2 (0.1 / 0.1 / 0.1) | 2.3 (0.2 / 0.1 / 0.1) | 4.2 (1.4 / 0.4 / 0.3) | 1131 | 7.7 |
| svg-dom-pancss | 4x | default | 6 (0.2 / 0.4 / 0.1) | 6.2 (0.1 / 0.4 / 0.1) | 7.1 (0.2 / 0.1 / 0.1) | 8 (1.4 / 0.5 / 0.6) | 1131 | 4.7 |
| svg-dom-pulsecss | 4x | default | 2.1 (0.1 / 0.1 / 0.1) | 2.5 (0.1 / 0.1 / 0.1) | 2.9 (0.1 / 0.3 / 0.3) | 5.6 (1.3 / 0.4 / 0.5) | 1131 | 4.1 |
| svg-dom-pulsecanvas | 4x | default | 9.2 (0.2 / 0.1 / 0.1) | 9.3 (0.1 / 0.1 / 0.1) | 8.2 (0.2 / 0 / 0) | 10 (1.4 / 0.5 / 0.6) | 1131 | 5.8 |
| svg-best | 4x | default | 8.8 (0.2 / 0.2 / 0.1) | 9.1 (0.2 / 0.2 / 0.1) | 8 (0.2 / 0 / 0) | 8.8 (1.5 / 0.7 / 0.3) | 1131 | 5.1 |
| canvas | 4x | default | 19.3 (0.6 / 0 / 0) | 23 (0.8 / 0 / 0) | 32.2 (1.2 / 0 / 0) | 19.4 (0.9 / 0 / 0) | 19 | 8.3 |
| canvas-lod | 4x | default | 17.8 (0.6 / 0 / 0) | 24.2 (0.4 / 0 / 0) | 34.9 (0.8 / 0 / 0) | 20.9 (0.7 / 0 / 0) | 19 | 8 |
| canvas-lod-2layer | 4x | default | 26.6 (0.6 / 0 / 0) | 30.5 (0.4 / 0 / 0) | 12.3 (0.3 / 0 / 0) | 23.6 (0.6 / 0 / 0) | 20 | 7.3 |
| canvas-lod-dpr2 | 4x | default | 11.7 (0.6 / 0 / 0) | 15.2 (0.5 / 0 / 0) | 19.6 (0.6 / 0 / 0) | 12.5 (0.5 / 0 / 0) | 19 | 7.2 |
| canvas-lod-dpr1 | 4x | default | 5.7 (0.6 / 0 / 0) | 3.8 (0.5 / 0 / 0) | 4.1 (0.6 / 0 / 0) | 3.3 (0.6 / 0 / 0) | 19 | 6.6 |
| solid-flow | 1x | default | 2 (0.7 / 0.2 / 0) | 3.5 (0.8 / 0.3 / 0) | 3.1 (0.2 / 0.5 / 0) | 6.6 (5.1 / 0.3 / 0.2) | 1826 | 26.3 |
| solid-flow-visible-only | 1x | default | 3.2 (1.6 / 0.3 / 0.1) | 5.3 (2.1 / 0.5 / 0.2) | 3.2 (0.3 / 0.4 / 0) | 7.3 (5.4 / 0.6 / 0.2) | 1258 | 21.9 |
| solid-flow | 4x | default | 3.6 (1.4 / 0.4 / 0) | 5.7 (1.5 / 0.5 / 0) | 4.4 (0.5 / 0.7 / 0) | 14.3 (11 / 0.6 / 0.6) | 1826 | 44.7 |
| solid-flow-visible-only | 4x | default | 4.9 (2.3 / 0.6 / 0.2) | 8.5 (3.4 / 0.9 / 0.4) | 4.4 (0.5 / 0.5 / 0) | 13.7 (10.1 / 1 / 0.5) | 1258 | 24.2 |
| svg-dom | 4x | --disable-gpu | 2.4 (0.2 / 0.1 / 0.1) | 2.5 (0.1 / 0.1 / 0.1) | 2.8 (0.2 / 0.2 / 0.1) | 5.8 (1.3 / 0.5 / 0.5) | 1131 | 4 |
| canvas-lod | 4x | --disable-gpu | 18 (0.6 / 0 / 0) | 21.4 (0.4 / 0 / 0) | 29.6 (0.6 / 0 / 0) | 18.9 (0.6 / 0 / 0) | 19 | 7.2 |

Layouts during "grow" (200 nodes): svg-solid 1x: 10 layouts, median 164.7 ms; svg-dom 1x: 10 layouts, median 169.4 ms; svg-best 1x: 10 layouts, median 168.4 ms; canvas-lod 1x: 10 layouts, median 170 ms

Cold start to first layout applied (worker spawn + ELK compile + layout), ms: svg-solid 1x: 409; svg-dom 1x: 387; svg-best 1x: 381; canvas-lod 1x: 381; canvas-lod-dpr2 1x: 392; svg-solid 4x: 423; svg-dom 4x: 922; svg-dom-cull 4x: 422; svg-dom-lod 4x: 418; svg-dom-pancss 4x: 396; svg-dom-pulsecss 4x: 422; svg-dom-pulsecanvas 4x: 398; svg-best 4x: 421; canvas 4x: 544; canvas-lod 4x: 387; canvas-lod-2layer 4x: 418; canvas-lod-dpr2 4x: 425; canvas-lod-dpr1 4x: 439 (layout: tree-order-noforce)

### 500 nodes / 800 edges: frame time (median / p95 ms, % of frames over 33 ms)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow |
| --- | --- | --- | --- | --- | --- | --- |
| svg-solid | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% |
| svg-dom | 1x | default | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| svg-best | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| canvas-lod | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% |
| canvas-lod-dpr2 | 1x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| svg-solid | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% |
| svg-dom | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% |
| svg-best | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% |
| canvas-lod | 4x | default | 16.7 / 33.4, 17.1% | 16.7 / 66.7, 37.7% | 33.3 / 33.4, 91.7% | 16.7 / 16.8, 3.5% |
| canvas-lod-dpr2 | 4x | default | 16.7 / 16.7, 1.5% | 16.7 / 50, 33.1% | 16.7 / 33.4, 34.6% | 16.7 / 16.7, 1.5% |
| solid-flow | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.7, 0.5% | 16.7 / 16.8, 0% | 16.7 / 116.7, 24.2% |
| solid-flow-visible-only | 4x | default | 16.7 / 33.3, 9.1% | 16.7 / 16.8, 1.4% | 16.7 / 16.7, 0% | 16.7 / 100, 15.3% |

### 500 nodes / 800 edges: main-thread time per frame (ms: total task, of which script / style / layout)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow | DOM elements | JS heap MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| svg-solid | 1x | default | 2.3 (0.3 / 0.1 / 0.1) | 2.9 (0.3 / 0.1 / 0.1) | 3.1 (0.4 / 0.1 / 0.1) | 0.8 (0.2 / 0 / 0) | 2831 | 7.5 |
| svg-dom | 1x | default | 2.3 (0.1 / 0.1 / 0.1) | 2.6 (0.1 / 0.1 / 0.1) | 3.1 (0.3 / 0.1 / 0.1) | 0.8 (0.2 / 0 / 0) | 2831 | 6.8 |
| svg-best | 1x | default | 5 (0.2 / 0.1 / 0.1) | 5.6 (0.2 / 0.1 / 0.1) | 5 (0.2 / 0 / 0) | 2.5 (0.2 / 0.1 / 0) | 2831 | 8.9 |
| canvas-lod | 1x | default | 7.8 (0.4 / 0 / 0) | 11.3 (0.3 / 0 / 0) | 9.5 (0.3 / 0 / 0) | 3.1 (0.1 / 0 / 0) | 19 | 7.6 |
| canvas-lod-dpr2 | 1x | default | 5.7 (0.5 / 0 / 0) | 8.7 (0.3 / 0 / 0) | 9.4 (0.5 / 0 / 0) | 2 (0.1 / 0 / 0) | 19 | 7.5 |
| svg-solid | 4x | default | 3.8 (0.5 / 0.2 / 0.2) | 5.5 (0.5 / 0.2 / 0.2) | 5.8 (0.7 / 0.3 / 0.1) | 1.8 (0.4 / 0.1 / 0.1) | 2831 | 6.9 |
| svg-dom | 4x | default | 4 (0.2 / 0.1 / 0.2) | 5.3 (0.1 / 0.1 / 0.2) | 5 (0.3 / 0.2 / 0.1) | 1.7 (0.3 / 0.1 / 0.1) | 2831 | 6.4 |
| svg-best | 4x | default | 10.4 (0.2 / 0.3 / 0.2) | 10.6 (0.2 / 0.2 / 0.1) | 9.9 (0.3 / 0 / 0) | 9.4 (0.4 / 0.1 / 0) | 2831 | 10.2 |
| canvas-lod | 4x | default | 18.5 (0.7 / 0 / 0) | 27.7 (0.4 / 0 / 0) | 36.2 (0.9 / 0 / 0) | 8.2 (0.2 / 0 / 0) | 19 | 7.8 |
| canvas-lod-dpr2 | 4x | default | 12 (0.7 / 0 / 0) | 22.1 (0.6 / 0 / 0) | 24.3 (0.8 / 0 / 0) | 5.7 (0.2 / 0 / 0) | 19 | 7.5 |
| solid-flow | 4x | default | 6.8 (1.9 / 0.5 / 0) | 10.2 (2 / 0.6 / 0) | 5.3 (0.9 / 0.7 / 0) | 20.9 (17 / 0.6 / 1) | 4726 | 66.9 |
| solid-flow-visible-only | 4x | default | 12 (6.4 / 0.9 / 0.5) | 12.3 (4.7 / 1.1 / 0.5) | 4.9 (1 / 0.4 / 0) | 17 (14.3 / 0.4 / 0.5) | 1356 | 108 |

Layouts during "grow" (500 nodes): svg-solid 1x: 2 layouts, median 2915.1 ms; svg-dom 1x: 2 layouts, median 2587.1 ms; svg-best 1x: 2 layouts, median 2582.2 ms; canvas-lod 1x: 2 layouts, median 2887.1 ms

Cold start to first layout applied (worker spawn + ELK compile + layout), ms: svg-solid 1x: 3183; svg-dom 1x: 3170; svg-best 1x: 3039; canvas-lod 1x: 2958; canvas-lod-dpr2 1x: 2965; svg-solid 4x: 3264; svg-dom 4x: 3379; svg-best 4x: 3275; canvas-lod 4x: 3590; canvas-lod-dpr2 4x: 3097 (layout: tree-order-noforce)

### 1000 nodes / 1600 edges: frame time (median / p95 ms, % of frames over 33 ms)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow |
| --- | --- | --- | --- | --- | --- | --- |
| svg-dom | 4x | default | 16.7 / 16.7, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 50, 12% |
| svg-best | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 16.8, 0% | 16.7 / 33.4, 10.1% |
| canvas-lod | 4x | default | 16.7 / 33.4, 29.7% | 16.8 / 83.3, 49.5% | 66.7 / 66.8, 100% | 66.6 / 83.3, 59% |

### 1000 nodes / 1600 edges: main-thread time per frame (ms: total task, of which script / style / layout)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow | DOM elements | JS heap MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| svg-dom | 4x | default | 5.5 (0.4 / 0.1 / 0.2) | 6.9 (0.3 / 0.1 / 0.2) | 6.9 (0.3 / 0.2 / 0.1) | 10.3 (2.5 / 0.7 / 1.2) | 5631 | 15.6 |
| svg-best | 4x | default | 10.8 (0.3 / 0.3 / 0.3) | 9.5 (0.2 / 0.2 / 0.1) | 7.9 (0.2 / 0 / 0) | 10.1 (3.2 / 1.4 / 0.2) | 5631 | 22.9 |
| canvas-lod | 4x | default | 23.1 (1.1 / 0 / 0) | 38.6 (0.8 / 0 / 0) | 83.9 (1.4 / 0 / 0) | 45.8 (1.4 / 0 / 0) | 19 | 11.3 |

Layouts during "grow" (1000 nodes): svg-dom 4x: 10 layouts, median 1 ms; svg-best 4x: 10 layouts, median 1.1 ms; canvas-lod 4x: 10 layouts, median 1.1 ms

Cold start to first layout applied (worker spawn + ELK compile + layout), ms: svg-dom 4x: 124; svg-best 4x: 99; canvas-lod 4x: 39 (layout: custom-tree-top)

### 2000 nodes / 3200 edges: frame time (median / p95 ms, % of frames over 33 ms)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow |
| --- | --- | --- | --- | --- | --- | --- |
| svg-dom | 4x | default | 16.7 / 16.7, 0.5% | 16.7 / 16.8, 1.5% | 16.7 / 16.7, 0.5% | 16.7 / 66.7, 18.2% |
| svg-best | 4x | default | 16.7 / 16.8, 0% | 16.7 / 16.7, 0% | 16.7 / 16.7, 0% | 16.7 / 33.5, 12.9% |
| canvas-lod | 4x | default | 33.3 / 50.1, 61.2% | 49.9 / 66.7, 75.9% | 66.6 / 66.7, 100% | 50 / 66.7, 50.7% |

### 2000 nodes / 3200 edges: main-thread time per frame (ms: total task, of which script / style / layout)

| renderer | CPU | browser flags | pan-close | pan-far | pulses | grow | DOM elements | JS heap MB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| svg-dom | 4x | default | 11.4 (0.6 / 0.1 / 0.3) | 14.8 (0.6 / 0.1 / 0.3) | 14.1 (0.3 / 0.2 / 0.1) | 12.8 (2.7 / 0.7 / 1.3) | 11231 | 16.1 |
| svg-best | 4x | default | 11.2 (0.2 / 0.4 / 0.4) | 9.7 (0.2 / 0.4 / 0.1) | 8 (0.2 / 0 / 0) | 11.1 (3.8 / 1.6 / 0.3) | 11231 | 30.4 |
| canvas-lod | 4x | default | 31.4 (1.7 / 0 / 0) | 44.1 (1.3 / 0 / 0) | 72.1 (2 / 0 / 0) | 36.9 (1.6 / 0 / 0) | 19 | 15.5 |

Layouts during "grow" (2000 nodes): svg-dom 4x: 10 layouts, median 1.7 ms; svg-best 4x: 10 layouts, median 1.9 ms; canvas-lod 4x: 10 layouts, median 1.5 ms

Cold start to first layout applied (worker spawn + ELK compile + layout), ms: svg-dom 4x: 201; svg-best 4x: 169; canvas-lod 4x: 66 (layout: custom-tree-top)

### Layout time (ms)

| nodes / edges | configuration | where | main-thread CPU | first run | full layout (best of 3 warm) | add one node (median / max of 6) |
| --- | --- | --- | --- | --- | --- | --- |
| 50 / 80 | layered-default | worker | 1x | 198.1 | 27.1 | 33.3 / 43.7 |
| 50 / 80 | layered-default | main | 1x | 106.9 | 32.1 | 38 / 44.1 |
| 50 / 80 | tree-order-noforce | worker | 1x | 39.9 | 15.8 | 11.3 / 13.6 |
| 50 / 80 | tree-order-noforce | main | 1x | 20 | 15.5 | 25.9 / 32.4 |
| 50 / 80 | mrtree-tree | worker | 1x | 28.3 | 14.3 | 6.9 / 7.4 |
| 50 / 80 | mrtree-tree | main | 1x | 28.3 | 24.9 | 16.6 / 16.8 |
| 50 / 80 | custom-tree-top | main | 1x | 0.1 | 0 | 0 / 0.1 |
| 50 / 80 | layered-default | worker | 4x | 202.3 | 31.3 | 27.2 / 34.7 |
| 50 / 80 | layered-default | main | 4x | 429.3 | 130.5 | 118.4 / 131.2 |
| 50 / 80 | tree-order-noforce | worker | 4x | 29.2 | 15.7 | 17 / 21.3 |
| 50 / 80 | tree-order-noforce | main | 4x | 85.4 | 66.6 | 60.9 / 70.3 |
| 50 / 80 | mrtree-tree | worker | 4x | 32.6 | 19.2 | 10.2 / 12.7 |
| 50 / 80 | mrtree-tree | main | 4x | 134.2 | 85.8 | 43.2 / 44.5 |
| 50 / 80 | custom-tree-top | main | 4x | 0.7 | 0 | 0 / 0.5 |
| 200 / 300 | layered-default | worker | 1x | 326.9 | 119.4 | 76.7 / 77.9 |
| 200 / 300 | layered-default | main | 1x | 228.8 | 124.8 | 99.5 / 105.6 |
| 200 / 300 | tree-order-noforce | worker | 1x | 185.5 | 161.3 | 158.9 / 182.3 |
| 200 / 300 | tree-order-noforce | main | 1x | 191.5 | 176 | 178.7 / 196.8 |
| 200 / 300 | mrtree-tree | worker | 1x | 86.2 | 43.6 | 23.5 / 24 |
| 200 / 300 | mrtree-tree | main | 1x | 77.6 | 69.8 | 34.7 / 36.4 |
| 200 / 300 | custom-tree-top | main | 1x | 0.2 | 0.1 | 0.1 / 0.1 |
| 200 / 300 | layered-default | worker | 4x | 339.7 | 107.1 | 97.9 / 104.3 |
| 200 / 300 | layered-default | main | 4x | 925 | 410.3 | 387.2 / 409 |
| 200 / 300 | tree-order-noforce | worker | 4x | 211.3 | 169.9 | 177.6 / 193.6 |
| 200 / 300 | tree-order-noforce | main | 4x | 808.9 | 726.9 | 702 / 713.1 |
| 200 / 300 | mrtree-tree | worker | 4x | 82.9 | 61.2 | 25.5 / 28.5 |
| 200 / 300 | mrtree-tree | main | 4x | 321.1 | 227.1 | 117.9 / 119.3 |
| 200 / 300 | custom-tree-top | main | 4x | 0.7 | 0.1 | 0.1 / 0.1 |
| 500 / 800 | layered-default | worker | 1x | 539 | 222.6 | 244.5 / 281.1 |
| 500 / 800 | layered-default | main | 1x | 489.3 | 257.4 | 253.4 / 274.1 |
| 500 / 800 | tree-order-noforce | worker | 1x | 2783.9 | 2745 | 2666.3 / 2720 |
| 500 / 800 | tree-order-noforce | main | 1x | 2867.3 | 2819.6 | 2781.9 / 2886.5 |
| 500 / 800 | mrtree-tree | worker | 1x | 258.1 | 150.1 | 75.9 / 99.1 |
| 500 / 800 | mrtree-tree | main | 1x | 191.7 | 159.8 | 82.3 / 92.5 |
| 500 / 800 | custom-tree-top | main | 1x | 0.3 | 0.1 | 0.2 / 0.2 |
| 500 / 800 | layered-default | worker | 4x | 579.4 | 238.2 | 232.1 / 239.8 |
| 500 / 800 | layered-default | main | 4x | 1799.2 | 1071.6 | 983.8 / 1023.2 |
| 500 / 800 | tree-order-noforce | worker | 4x | 2958.1 | 3096.5 | 2831.3 / 2909.3 |
| 500 / 800 | tree-order-noforce | main | 4x | 11894.6 | 11578.9 | 11234.2 / 11597.6 |
| 500 / 800 | mrtree-tree | worker | 4x | 281.5 | 135.5 | 77 / 90.2 |
| 500 / 800 | mrtree-tree | main | 4x | 722.7 | 609.8 | 318.6 / 337.5 |
| 500 / 800 | custom-tree-top | main | 4x | 1.9 | 0.8 | 0.5 / 0.8 |

### Stability: displacement of existing nodes per added node (px)

| nodes / edges | configuration | mean | mean, pan-compensated | max per step: mean | max per step: p95 | max per step: worst | nodes moved > 2 px | steps with no movement | layout ms (Node, mean) | final size | overlapping pairs |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 50 / 80 | layered-default | 162 | 155 | 451.2 | 972.7 | 1135.3 | 74.8% | 6.3% | 21.8 | 1060x1018 | 0 |
| 50 / 80 | tree-model-order-leftup | 32 | 24.3 | 93.1 | 308 | 364.3 | 29.3% | 29.2% | 13.2 | 848x1319 | 0 |
| 50 / 80 | mrtree-tree | 16.7 | 12.2 | 109 | 898.6 | 1294.7 | 23.6% | 35.4% | 8.9 | 1168x1182 | 0 |
| 50 / 80 | tree-order-noforce | 20.2 | 10.4 | 41.7 | 297.2 | 308 | 15.9% | 33.3% | 9 | 836x1344 | 0 |
| 50 / 80 | custom-tree-top | 11 | 8.2 | 27.3 | 160 | 188 | 13.1% | 56.3% | 0 | 972x1308 | 0 |
| 200 / 300 | tree-model-order-nocm | 8.7 | 6 | 50.6 | 238.7 | 856.7 | 26.4% | 14.6% | 57.2 | 1340x5893 | 0 |
| 200 / 300 | tree-order-noforce | 11.6 | 7.3 | 36.3 | 110.3 | 474.8 | 16.9% | 19.7% | 54.5 | 836x5669 | 0 |
| 200 / 300 | layered-default | 766.4 | 741.3 | 2259.6 | 4542.1 | 5112 | 90.7% | 4% | 43 | 1810x4495 | 0 |
| 200 / 300 | layered-nosep | 721.1 | 696 | 2189.3 | 4363.6 | 5050.2 | 92.5% | 1% | 38.9 | 1792x4463 | 0 |
| 200 / 300 | model-order | 157.1 | 134.7 | 870.2 | 3311 | 4603.7 | 64.7% | 6.6% | 154.1 | 2560x5758 | 0 |
| 200 / 300 | model-order-leftup | 121.6 | 100.9 | 648.4 | 1927.7 | 5127.6 | 64% | 6.1% | 152 | 2722x6009 | 0 |
| 200 / 300 | model-order-linear | 89.8 | 67.4 | 388.7 | 1444.6 | 2055.5 | 80.4% | 4% | 166.6 | 2842x3933 | 0 |
| 200 / 300 | model-order-simplex | 60.6 | 47.8 | 328.4 | 1210 | 2584 | 67.6% | 6.1% | 241.1 | 2530x2674 | 0 |
| 200 / 300 | model-order-simple | 40.2 | 30.2 | 197.3 | 1039.7 | 1537 | 71.8% | 0% | 151.6 | 3004x2653 | 0 |
| 200 / 300 | interactive | 196.1 | 160.7 | 813.3 | 1933.6 | 2912.5 | 85.5% | 4.5% | 43.4 | 6270x6087 | 0 |
| 200 / 300 | interactive-leftup | 148.7 | 121 | 664.8 | 2330.3 | 2585.3 | 73% | 8.6% | 41.7 | 6198x6680 | 0 |
| 200 / 300 | interactive-placer | 69.3 | 30.7 | 102 | 274.3 | 514 | 68.2% | 6.1% | 68.5 | 15934x2350 | 0 |
| 200 / 300 | semi-interactive | 105.5 | 87.2 | 720.1 | 2524.3 | 3126.6 | 56.2% | 9.6% | 55.9 | 2230x4775 | 0 |
| 200 / 300 | mrtree | 16.2 | 12.2 | 107.4 | 929.7 | 1732.6 | 25.5% | 17.7% | 22.4 | 932x5324 | 0 |
| 200 / 300 | stress-fixed | 7.3 | 5.3 | 44.9 | 306.3 | 1740.2 | 2.3% | 90.4% | 57.8 | 2159x1397 | 229 |
| 200 / 300 | tree-default | 547.7 | 525.5 | 2146.9 | 4176.1 | 5511.3 | 83.2% | 1% | 20.9 | 836x5615 | 0 |
| 200 / 300 | tree-model-order | 14 | 10.3 | 71.5 | 410 | 927.7 | 28.1% | 17.7% | 115.9 | 1340x5893 | 0 |
| 200 / 300 | tree-model-order-leftup | 10.5 | 7.5 | 60 | 333.3 | 856.7 | 27.6% | 11.6% | 99.3 | 1340x5893 | 0 |
| 200 / 300 | tree-model-order-linear | 16.5 | 11.4 | 79.5 | 233.5 | 547.3 | 42.7% | 4% | 122.4 | 1418x5268 | 0 |
| 200 / 300 | tree-model-order-simplex | 10.5 | 8 | 58.1 | 238 | 599 | 23.1% | 20.7% | 110 | 1346x5268 | 0 |
| 200 / 300 | tree-model-order-simple | 21.1 | 14 | 40.3 | 104.1 | 364.3 | 41% | 0% | 90.3 | 1520x5268 | 0 |
| 200 / 300 | tree-interactive | 15.2 | 9.4 | 65.9 | 188 | 2483.3 | 24.4% | 16.7% | 16.6 | 1216x5503 | 0 |
| 200 / 300 | tree-changed-model-order-leftup | 37.7 | 32.1 | 180.1 | 606.6 | 1476.6 | 53.8% | 7.1% | 151.2 | 1704x5772 | 0 |
| 200 / 300 | mrtree-tree | 14.8 | 10.8 | 95.3 | 530.2 | 2316.4 | 23.6% | 16.2% | 14.7 | 1696x5325 | 0 |
| 200 / 300 | custom-tree-top | 11.5 | 7.5 | 28.2 | 52 | 188 | 16% | 37.9% | 0 | 972x6104 | 0 |
| 200 / 300 | custom-tree-center | 12.2 | 7.8 | 30.5 | 52 | 188 | 18.7% | 27.3% | 0 | 972x6104 | 0 |
| 500 / 800 | layered-default | 1800.9 | 1751.9 | 5728.8 | 11800.9 | 13371.7 | 96.6% | 1.2% | 118.3 | 2728x11791 | 0 |
| 500 / 800 | mrtree-tree | 14 | 8.5 | 61.3 | 74 | 2316.4 | 21.4% | 7.8% | 38.4 | 1696x15472 | 0 |
| 500 / 800 | custom-tree-top | 12.9 | 7.4 | 31 | 52 | 188 | 17% | 27.9% | 0.1 | 972x16854 | 0 |

### Trace breakdown, 200 nodes, main thread throttled 4x (ms per frame)

| renderer | scenario | frame median / p95 | script | style | layout | pre-paint | paint | layerize | commit | raster threads (not throttled) | raster tasks |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| svg-dom | pan-close | 16.7 / 16.7 | 0.7 | 0.15 | 0.36 | 0.4 | 0.02 | 1.14 | 0.25 | 2.66 | 1448 |
| svg-dom | pan-far | 16.7 / 16.7 | 0.56 | 0.16 | 0.33 | 0.35 | 0.03 | 1.48 | 0.18 | 2.55 | 1192 |
| svg-dom | pulses | 16.7 / 16.7 | 1.08 | 0.28 | 0.31 | 0.38 | 0.05 | 1.71 | 0.24 | 0.69 | 798 |
| svg-dom | grow | 16.7 / 16.8 | 3.4 | 0.52 | 0.69 | 0.37 | 3.88 | 0.86 | 0.24 | 1.86 | 1460 |
| svg-dom-cull | pan-close | 16.7 / 16.7 | 0.64 | 0.18 | 0.38 | 0.42 | 0.08 | 0.53 | 0.19 | 2.67 | 1448 |
| svg-dom-cull | pan-far | 16.7 / 16.7 | 0.52 | 0.25 | 0.45 | 0.41 | 0.2 | 1.18 | 0.19 | 2.42 | 1192 |
| svg-dom-cull | pulses | 16.7 / 16.8 | 1 | 0.3 | 0.32 | 0.49 | 0.08 | 1.45 | 0.19 | 0.64 | 797 |
| svg-dom-cull | grow | 16.7 / 16.7 | 3.45 | 0.68 | 0.9 | 0.47 | 3.32 | 0.78 | 0.25 | 1.89 | 1480 |
| svg-dom-lod | pan-close | 16.7 / 16.8 | 0.61 | 0.19 | 0.32 | 0.33 | 0.07 | 0.99 | 0.16 | 2.67 | 1448 |
| svg-dom-lod | pan-far | 16.7 / 16.7 | 0.53 | 0.22 | 0.19 | 0.19 | 0.01 | 1.45 | 0.2 | 1.68 | 1192 |
| svg-dom-lod | pulses | 16.7 / 16.8 | 0.78 | 0.24 | 0.19 | 0.23 | 0.02 | 1.3 | 0.17 | 0.47 | 800 |
| svg-dom-lod | grow | 16.7 / 16.7 | 3.08 | 0.49 | 0.29 | 0.25 | 0.89 | 0.76 | 0.24 | 1.15 | 1484 |
| svg-dom-pancss | pan-close | 16.7 / 16.7 | 0.89 | 0.5 | 0.33 | 0.41 | 0.02 | 4.97 | 0.24 | 7.15 | 4102 |
| svg-dom-pancss | pan-far | 16.7 / 16.7 | 0.61 | 0.43 | 0.27 | 0.39 | 0.01 | 4.72 | 0.23 | 5 | 2097 |
| svg-dom-pancss | pulses | 16.7 / 16.7 | 0.72 | 0.27 | 0.27 | 0.33 | 0.04 | 5.77 | 0.31 | 1.22 | 1128 |
| svg-dom-pancss | grow | 16.7 / 16.8 | 3.35 | 0.47 | 0.61 | 0.35 | 3.51 | 2.78 | 0.33 | 2.69 | 1908 |
| svg-cull-lod | pan-close | 16.7 / 16.8 | 0.66 | 0.25 | 0.32 | 0.38 | 0.09 | 0.56 | 0.19 | 2.39 | 1448 |
| svg-cull-lod | pan-far | 16.7 / 16.8 | 0.57 | 0.22 | 0.26 | 0.23 | 0.07 | 1.05 | 0.22 | 1.62 | 1192 |
| svg-cull-lod | pulses | 16.7 / 16.8 | 1.13 | 0.31 | 0.22 | 0.27 | 0.03 | 1.45 | 0.26 | 0.54 | 802 |
| svg-cull-lod | grow | 16.7 / 16.7 | 3.41 | 0.67 | 0.28 | 0.27 | 0.91 | 0.69 | 0.22 | 1.17 | 1485 |
| canvas-lod | pan-close | 16.7 / 33.4 | 1.74 | 0 | 0.01 | 0.16 | 0 | 0.01 | 16.86 | 0 | 0 |
| canvas-lod | pan-far | 16.7 / 33.4 | 1.18 | 0 | 0.01 | 0.19 | 0 | 0.01 | 19.86 | 0 | 0 |
| canvas-lod | pulses | 33.3 / 33.4 | 1.87 | 0 | 0 | 0.18 | 0 | 0.02 | 29.03 | 0 | 0 |
| canvas-lod | grow | 16.7 / 33.4 | 1.57 | 0 | 0 | 0.13 | 0 | 0.01 | 16.67 | 0 | 0 |
