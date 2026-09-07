import { memo } from "react";
import { gridStyle, WORLD_SPAN } from "@/features/layout/geometry";

// The grid in world coordinates, drawn at 1 screen px at any zoom. The
// screens sit on top of this; the grid stays deliberately quiet so the
// screens read as solid surfaces.
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