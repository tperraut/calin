#!/usr/bin/env bash
# Every measurement behind README.md, in order. Takes about 50 minutes; keep the machine idle.
#   vp build && bash scripts/run-all.sh
# Frame-time and layout results land in results/*.json; `node scripts/report.mjs` prints the tables.
cd "$(dirname "$0")/.." || exit 1
rm -f results/render-*.json results/layout.json
SUBSET=svg-solid,svg-dom,svg-best,canvas-lod,canvas-lod-dpr2

node scripts/measure.mjs gpuinfo
SPIKE_DISABLE_GPU=1 node scripts/measure.mjs gpuinfo

node scripts/measure.mjs render 200 300 1 "$SUBSET"
node scripts/measure.mjs render 200 300 4
node scripts/measure.mjs flow 200 300 1
node scripts/measure.mjs flow 200 300 4
SPIKE_DISABLE_GPU=1 node scripts/measure.mjs render 200 300 4 svg-dom,canvas-lod

for size in "50 80" "500 800"; do
  node scripts/measure.mjs render $size 1 "$SUBSET"
  node scripts/measure.mjs render $size 4 "$SUBSET"
done
node scripts/measure.mjs flow 500 800 4
# Past the target, to find where each renderer stops being smooth.
# ELK is out of these two (hand-written tree layout): with the model-order options it needs
# about 20 s per layout at 1000 nodes and did not finish within 3 minutes at 2000.
node scripts/measure.mjs render 1000 1600 4 svg-dom,svg-best,canvas-lod cfg=custom-tree-top
node scripts/measure.mjs render 2000 3200 4 svg-dom,svg-best,canvas-lod cfg=custom-tree-top
node scripts/trace.mjs 200 300 4

for size in "50 80" "200 300" "500 800"; do
  node scripts/measure.mjs layout $size 1
  node scripts/measure.mjs layout $size 4
done

# Deterministic and CPU-only, so last: it must not run next to the frame-time measurements.
PICK=layered-default,tree-model-order-leftup,tree-order-noforce,mrtree-tree,custom-tree-top
node scripts/stability.ts 50 80 "$PICK" 2>/dev/null | tail -n 12
# tree-model-order-leftup is left out at 500: several seconds per layout, 500 layouts.
node scripts/stability.ts 500 800 layered-default,tree-order-noforce,mrtree-tree,custom-tree-top 2>/dev/null | tail -n 12
echo ALL-DONE
