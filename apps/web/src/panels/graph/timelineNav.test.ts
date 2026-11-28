import { describe, expect, it } from "vite-plus/test";
import { AXIS_HEIGHT, LANE_HEIGHT } from "./timelineModel";
import { laneColumnWidth, revealScroll } from "./timelineNav";

describe("the lane column", () => {
  it("is narrow on a measured screen below 520 px, wide from 520 px or before measuring", () => {
    expect(laneColumnWidth(1)).toBe(104);
    expect(laneColumnWidth(519)).toBe(104);
    expect(laneColumnWidth(520)).toBe(176);
    expect(laneColumnWidth(0)).toBe(176);
  });
});

describe("revealing a point of the chart", () => {
  const view = { left: 1000, top: 0, width: 600, height: 300 };

  it("keeps the scroll while the point is 16 px past the left edge and 48 px before the right one", () => {
    expect(revealScroll(1016, 0, view).left).toBe(1000);
    expect(revealScroll(1552, 0, view).left).toBe(1000);
  });

  it("puts a point closer to either edge a third of the way in", () => {
    expect(revealScroll(1010, 0, view).left).toBe(810);
    expect(revealScroll(1560, 0, view).left).toBe(1360);
  });

  it("on a viewport shorter than a lane and the axis, shows the lane's bottom even when its top is at the edge", () => {
    const short = { ...view, top: 2 * LANE_HEIGHT, height: 50 };
    expect(revealScroll(1200, 2, short).top).toBe(3 * LANE_HEIGHT + AXIS_HEIGHT - 50);
  });
});
