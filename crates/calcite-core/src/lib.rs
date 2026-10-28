//! Shared domain types that do not depend on transport or persistence.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Uuid(pub [u8; 16]);

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                write!(f, "-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("entity pool is full")]
    PoolFull,
    #[error("entity is no longer alive")]
    EntityDead,
}
