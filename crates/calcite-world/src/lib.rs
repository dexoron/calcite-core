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
    pub fn block_index(pos: BlockPos) -> Option<usize> {
        if !(MIN_Y..MAX_Y).contains(&pos.y)
            || !(0..16).contains(&(pos.x & 15))
            || !(0..16).contains(&(pos.z & 15))
        {
            return None;
        }
        let y = (pos.y - MIN_Y) as usize;
        Some((y * 256) + ((pos.z & 15) as usize * 16) + (pos.x & 15) as usize)
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
