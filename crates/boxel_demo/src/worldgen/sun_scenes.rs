//! Small scenes for the sun shadows (ADR 0065), built on flat ground: [`SunScene::House`], a
//! room with a south window under its eaves; [`SunScene::CaveMouth`], a tunnel into a hill that
//! opens to the west, where the afternoon sun reaches into it; and [`SunScene::Lake`], a pond
//! with two trees on its shore whose shadows fall on the water. Each is a list of boxes of
//! blocks ([`SceneBox`], a later box replacing the cells of an earlier one) and a camera, so
//! `snapshot --preset` sets them as `--fill` edits.

use boxel_voxel::BlockPos;

/// The scene's walls.
pub const WALL: &str = "boxel:cobblestone";
/// The house's floor and roof.
pub const PLANKS: &str = "boxel:planks";
/// The house's window: glass casts no shadow, so the sun comes in through it.
pub const GLASS: &str = "boxel:glass";
/// The hill of the cave mouth.
pub const STONE: &str = "boxel:stone";
/// The top of the hill.
pub const GRASS: &str = "boxel:grass";
/// The pond's bed.
pub const SAND: &str = "boxel:sand";
/// The pond (sources).
pub const WATER: &str = "boxel:water";
/// The trees' trunks.
pub const LOG: &str = "boxel:log";
/// The trees' crowns.
pub const LEAVES: &str = "boxel:leaves";
/// Every carved cell.
pub const AIR: &str = "boxel:air";

/// A box of one block, both corners inclusive (world cells).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneBox {
    /// The block's name.
    pub block: &'static str,
    /// The low corner.
    pub min: BlockPos,
    /// The high corner.
    pub max: BlockPos,
}

impl SceneBox {
    /// Whether `p` is in the box.
    pub fn contains(&self, p: BlockPos) -> bool {
        (self.min.x..=self.max.x).contains(&p.x)
            && (self.min.y..=self.max.y).contains(&p.y)
            && (self.min.z..=self.max.z).contains(&p.z)
    }

    /// The cells of the box.
    pub fn volume(&self) -> u64 {
        [
            self.max.x - self.min.x,
            self.max.y - self.min.y,
            self.max.z - self.min.z,
        ]
        .iter()
        .map(|d| d.unsigned_abs() + 1)
        .product()
    }
}

/// A scene for the sun shadows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SunScene {
    /// A cobblestone room of 7 by 7 and 5 high with a planks floor, a roof one block past its
    /// walls but the south one, and glass windows 5 wide and 4 high in its south wall (the side
    /// the sun leans to: lit all day, the patch on the floor always 0.58 blocks deep per block
    /// of height) and its west wall (dark in the morning, lit deeper and deeper through the
    /// afternoon). The camera stands in the north-east corner, looking at both.
    House,
    /// A stone hill of 17 by 17 and 8 high with a grassy top and a tunnel 3 wide and 3 high into
    /// it from its west side, where the afternoon sun shines in; the camera stands at the end of
    /// the tunnel, looking out.
    CaveMouth,
    /// A pond of water two blocks deep on sand, with a tree on its west shore and another on its
    /// south shore whose crowns lean over the water; the camera looks at both from the north-east
    /// shore.
    Lake,
}

impl SunScene {
    /// Every scene, in order.
    pub const ALL: [SunScene; 3] = [SunScene::House, SunScene::CaveMouth, SunScene::Lake];

    /// The scene's name (`house`, `cave-mouth`, `lake`).
    pub fn name(self) -> &'static str {
        match self {
            SunScene::House => "house",
            SunScene::CaveMouth => "cave-mouth",
            SunScene::Lake => "lake",
        }
    }

    /// The scene named `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The scene's boxes around `ground`, the top block of the flat ground under its center, in
    /// order: a later box replaces the cells of an earlier one.
    pub fn boxes(self, ground: BlockPos) -> Vec<SceneBox> {
        let at = |block, min: [i64; 3], max: [i64; 3]| SceneBox {
            block,
            min: BlockPos::new(ground.x + min[0], ground.y + min[1], ground.z + min[2]),
            max: BlockPos::new(ground.x + max[0], ground.y + max[1], ground.z + max[2]),
        };
        match self {
            SunScene::House => vec![
                at(WALL, [-4, 1, -4], [4, 5, 4]),
                at(AIR, [-3, 1, -3], [3, 5, 3]),
                at(PLANKS, [-3, 0, -3], [3, 0, 3]),
                // Four rows from the floor up, in the south (+z) and west (-x) walls.
                at(GLASS, [-2, 1, 4], [2, 4, 4]),
                at(GLASS, [-4, 1, -2], [-4, 4, 2]),
                // Eaves on three sides; flush over the window, so the sun reaches the floor.
                at(PLANKS, [-5, 6, -5], [5, 6, 4]),
            ],
            SunScene::CaveMouth => vec![
                at(STONE, [-8, 1, -8], [8, 7, 8]),
                at(GRASS, [-8, 8, -8], [8, 8, 8]),
                // Open at the west face (-x), ending 4 blocks short of the east one.
                at(AIR, [-8, 1, -1], [4, 3, 1]),
            ],
            SunScene::Lake => {
                let mut boxes = Vec::new();
                // A rounded pond: a cross of two boxes.
                for (hx, hz) in [(7, 4), (4, 7)] {
                    boxes.push(at(SAND, [-hx, -3, -hz], [hx, -3, hz]));
                    boxes.push(at(WATER, [-hx, -2, -hz], [hx, -1, hz]));
                    boxes.push(at(AIR, [-hx, 0, -hz], [hx, 0, hz]));
                }
                // Trunks 5 high, crowns of 5 by 5 by 3 and a cap of 3 by 3.
                for (x, z) in [(-9, 3), (0, 9)] {
                    boxes.push(at(LEAVES, [x - 2, 4, z - 2], [x + 2, 6, z + 2]));
                    boxes.push(at(LEAVES, [x - 1, 7, z - 1], [x + 1, 7, z + 1]));
                    boxes.push(at(LOG, [x, 1, z], [x, 5, z]));
                }
                boxes
            }
        }
    }

    /// The camera around `ground`: x, y, z, yaw and pitch (degrees, as `snapshot --camera`: yaw
    /// 0 looks north along -z, -90 east along +x, 90 west, 180 south).
    pub fn camera(self, ground: BlockPos) -> [f64; 5] {
        let eye = |x: f64, y: f64, z: f64| {
            [
                ground.x as f64 + x,
                ground.y as f64 + y,
                ground.z as f64 + z,
            ]
        };
        let ([x, y, z], yaw, pitch) = match self {
            // Standing on the floor (row 0): the eye 1.6 above it.
            SunScene::House => (eye(2.5, 2.6, -2.5), 135.0, -30.0),
            SunScene::CaveMouth => (eye(3.5, 2.6, 0.5), 90.0, -8.0),
            SunScene::Lake => (eye(10.5, 5.5, -9.5), 135.0, -22.0),
        };
        [x, y, z, yaw, pitch]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block of the last box holding `p`, `None` where no box does.
    fn block_at(boxes: &[SceneBox], p: BlockPos) -> Option<&'static str> {
        boxes.iter().rev().find(|b| b.contains(p)).map(|b| b.block)
    }

    #[test]
    fn names_round_trip() {
        for scene in SunScene::ALL {
            assert_eq!(SunScene::from_name(scene.name()), Some(scene));
        }
        assert_eq!(SunScene::from_name("castle"), None);
    }

    #[test]
    fn every_scene_builds_from_demo_blocks_with_the_camera_in_air() {
        let registry = crate::blocks::registry();
        for ground in [
            BlockPos::new(0, 63, 0),
            BlockPos::new(1 << 40, -(1 << 31), -(1 << 35)),
        ] {
            for scene in SunScene::ALL {
                let boxes = scene.boxes(ground);
                for b in &boxes {
                    assert!(
                        registry.by_name(b.block).is_some(),
                        "{}: {}",
                        scene.name(),
                        b.block
                    );
                    assert!(
                        b.min.x <= b.max.x && b.min.y <= b.max.y && b.min.z <= b.max.z,
                        "{b:?}"
                    );
                }
                let cells: u64 = boxes.iter().map(SceneBox::volume).sum();
                assert!(cells < 10_000, "{}: {cells} cells", scene.name());
                // The eye and the cell under the feet: air (carved or above the ground) over
                // a solid floor.
                let [x, y, z, ..] = scene.camera(ground);
                let eye = BlockPos::new(x.floor() as i64, y.floor() as i64, z.floor() as i64);
                let air = |p: BlockPos| block_at(&boxes, p).map_or(p.y > ground.y, |b| b == AIR);
                assert!(
                    air(eye),
                    "{}: eye in {:?}",
                    scene.name(),
                    block_at(&boxes, eye)
                );
                if scene != SunScene::Lake {
                    let feet = BlockPos::new(eye.x, ground.y + 1, eye.z);
                    let floor = BlockPos::new(eye.x, ground.y, eye.z);
                    assert!(air(feet) && !air(floor), "{}", scene.name());
                }
            }
        }
    }

    #[test]
    fn the_house_windows_face_south_and_west_and_the_cave_opens_west() {
        let g = BlockPos::new(0, 63, 0);
        let house = SunScene::House.boxes(g);
        // The south wall (+z, where the 30 degree tilt leans the sun) and the west one have the
        // glass; the north and east ones are closed.
        assert_eq!(
            block_at(&house, BlockPos::new(0, 65, 4)),
            Some(GLASS),
            "window"
        );
        assert_eq!(block_at(&house, BlockPos::new(0, 65, -4)), Some(WALL));
        assert_eq!(block_at(&house, BlockPos::new(-4, 65, 0)), Some(GLASS));
        assert_eq!(block_at(&house, BlockPos::new(4, 65, 0)), Some(WALL));
        // The roof: eaves past the north wall, flush with the south one.
        assert_eq!(block_at(&house, BlockPos::new(0, 69, -5)), Some(PLANKS));
        assert_eq!(block_at(&house, BlockPos::new(0, 69, 4)), Some(PLANKS));
        assert_eq!(block_at(&house, BlockPos::new(0, 69, 5)), None);
        let cave = SunScene::CaveMouth.boxes(g);
        assert_eq!(
            block_at(&cave, BlockPos::new(-8, 65, 0)),
            Some(AIR),
            "mouth"
        );
        assert_eq!(block_at(&cave, BlockPos::new(8, 65, 0)), Some(STONE));
        let lake = SunScene::Lake.boxes(g);
        assert_eq!(block_at(&lake, BlockPos::new(0, 62, 0)), Some(WATER));
        assert_eq!(block_at(&lake, BlockPos::new(-9, 66, 3)), Some(LOG));
        // A crown leans over the water.
        assert_eq!(block_at(&lake, BlockPos::new(-7, 67, 3)), Some(LEAVES));
        assert_eq!(block_at(&lake, BlockPos::new(-7, 62, 3)), Some(WATER));
    }
}
