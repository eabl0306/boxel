//! The shapes of the demo blocks made of boxes (ADR 0020, ADR 0027): stairs, fences, torches,
//! walls and fence gates, their boxes, outlines and collisions, and the properties they are
//! built from. The engine's model boxes are mechanism; these are content.

pub use boxel_voxel::shapes::{
    FenceSides, GATE_IN_WALL_DROP, StairShape, TORCH_BOX, TORCH_OUTLINE, WallSide, WallSides,
    fence_boxes, fence_collision, fence_gate_boxes, fence_gate_collision, fence_gate_outline,
    fence_outline, stair_boxes, wall_boxes, wall_collision, wall_outline, wall_torch_box,
    wall_torch_outline,
};
