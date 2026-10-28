use calcite_core::{CoreError, Uuid};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerState {
    Alive,
    Dying,
    Dead,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub entity_id: u32,
    pub uuid: Uuid,
    pub name: String,
    pub state: PlayerState,
}

pub struct PlayerPool {
    players: Vec<Option<Player>>,
}

impl PlayerPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            players: vec![None; capacity],
        }
    }
    pub fn insert(&mut self, player: Player) -> Result<usize, CoreError> {
        let slot = self
            .players
            .iter()
            .position(Option::is_none)
            .ok_or(CoreError::PoolFull)?;
        self.players[slot] = Some(player);
        Ok(slot)
    }
    pub fn iter(&self) -> impl Iterator<Item = &Player> {
        self.players.iter().filter_map(Option::as_ref)
    }
    pub fn len(&self) -> usize {
        self.players.iter().flatten().count()
    }
}
