use calcite_core::{BlockPos, ChunkPos};

pub const MIN_Y: i32 = -64;
pub const MAX_Y: i32 = 320;
pub const SECTION_COUNT: usize = 24;

#[derive(Clone, Debug)]
pub struct Chunk {
    pub position: ChunkPos,
    blocks: Vec<u32>,
}

impl Chunk {
    pub fn new(position: ChunkPos) -> Self {
        Self {
            position,
            blocks: vec![0; 16 * 16 * 16 * SECTION_COUNT],
        }
    }
    pub fn block_index(&self, pos: BlockPos) -> Option<usize> {
        let origin_x = i64::from(self.position.x) * 16;
        let origin_z = i64::from(self.position.z) * 16;
        let x = i64::from(pos.x) - origin_x;
        let z = i64::from(pos.z) - origin_z;
        if !(MIN_Y..MAX_Y).contains(&pos.y) || !(0..16).contains(&x) || !(0..16).contains(&z) {
            return None;
        }
        self.local_block_index(x as u8, pos.y, z as u8)
    }

    /// Returns the dense storage index for a chunk-local X/Z coordinate.
    /// The local coordinates must be in `0..16`; Y uses the world range.
    pub fn local_block_index(&self, x: u8, y: i32, z: u8) -> Option<usize> {
        if x >= 16 || z >= 16 || !(MIN_Y..MAX_Y).contains(&y) {
            return None;
        }
        let y = (y - MIN_Y) as usize;
        Some((y * 256) + (z as usize * 16) + x as usize)
    }
    pub fn set_block(&mut self, pos: BlockPos, state: u32) -> bool {
        self.block_index(pos)
            .map(|i| self.blocks[i] = state)
            .is_some()
    }
    pub fn block(&self, pos: BlockPos) -> Option<u32> {
        self.block_index(pos).map(|i| self.blocks[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_starts_with_air_and_has_expected_height() {
        let chunk = Chunk::new(ChunkPos { x: 3, z: -2 });

        assert_eq!(
            chunk.block(BlockPos {
                x: 48,
                y: MIN_Y,
                z: -32
            }),
            Some(0)
        );
        assert_eq!(
            chunk.block(BlockPos {
                x: 63,
                y: MAX_Y - 1,
                z: -17
            }),
            Some(0)
        );
        assert_eq!(
            chunk.block(BlockPos {
                x: 48,
                y: MAX_Y,
                z: -32
            }),
            None
        );
    }

    #[test]
    fn block_updates_are_limited_to_valid_vertical_range() {
        let mut chunk = Chunk::new(ChunkPos { x: 0, z: 0 });
        let pos = BlockPos { x: 4, y: 100, z: 9 };

        assert!(chunk.set_block(pos, 42));
        assert_eq!(chunk.block(pos), Some(42));
        assert!(!chunk.set_block(
            BlockPos {
                y: MIN_Y - 1,
                ..pos
            },
            7
        ));
        assert!(!chunk.set_block(BlockPos { y: MAX_Y, ..pos }, 7));
    }

    #[test]
    fn global_coordinates_map_to_chunk_local_storage() {
        let mut chunk = Chunk::new(ChunkPos { x: -2, z: 3 });
        let positive = BlockPos {
            x: -17,
            y: 0,
            z: 63,
        };

        assert!(chunk.set_block(positive, 99));
        assert_eq!(chunk.block(positive), Some(99));
        assert_eq!(chunk.block(BlockPos { x: -1, y: 0, z: 63 }), None);
    }

    #[test]
    fn adjacent_chunks_do_not_alias_coordinates() {
        let mut chunk = Chunk::new(ChunkPos { x: 0, z: 0 });
        let edge = BlockPos { x: 15, y: 0, z: 15 };

        assert!(chunk.set_block(edge, 99));
        assert_eq!(chunk.block(BlockPos { x: 16, ..edge }), None);
    }

    #[test]
    fn local_index_matches_global_index() {
        let chunk = Chunk::new(ChunkPos { x: -2, z: 3 });
        let global = BlockPos {
            x: -17,
            y: 12,
            z: 63,
        };
        assert_eq!(
            chunk.block_index(global),
            chunk.local_block_index(15, global.y, 15)
        );
        assert_eq!(chunk.local_block_index(16, 12, 0), None);
        assert_eq!(chunk.local_block_index(0, MAX_Y, 0), None);
    }
}
