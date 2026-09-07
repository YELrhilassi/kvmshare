import { memo } from "react";
import { gridStyle, WORLD_SPAN } from "@/features/layout/geometry";

// The dot grid in world coordinates, adaptive at every zoom. The screens
// sit on top and blur it away behind them, so the canvas reads as a calm
// surface with a faint texture rather than a wall of lines.
function GridLayer({ scale }: { scale: number }) {
  const style = gridStyle(scale);
  return (
    <div
      className="absolute"
      style={{
        left: -WORLD_SPAN,
        top: -WORLD_SPAN,
        width: WORLD_SPAN * 2,
        height: WORLD_SPAN * 2,
        backgroundImage: style.backgroundImage,
        backgroundSize: style.backgroundSize,
      }}
    />
  );
}

export default memo(GridLayer);