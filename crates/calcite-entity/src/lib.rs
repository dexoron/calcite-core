use calcite_core::{CoreError, Uuid};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerState {
    Alive,
    Dying,
    Dead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub fn remove(&mut self, slot: usize) -> Result<Player, CoreError> {
        self.players
            .get_mut(slot)
            .and_then(Option::take)
            .ok_or(CoreError::EntityDead)
    }
    pub fn iter(&self) -> impl Iterator<Item = &Player> {
        self.players.iter().filter_map(Option::as_ref)
    }
    pub fn len(&self) -> usize {
        self.players.iter().flatten().count()
    }
    pub fn is_empty(&self) -> bool {
        self.players.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(id: u32) -> Player {
        Player {
            entity_id: id,
            uuid: Uuid([id as u8; 16]),
            name: format!("player-{id}"),
            state: PlayerState::Alive,
        }
    }

    #[test]
    fn capacity_and_slot_reuse_are_deterministic() {
        let mut pool = PlayerPool::new(2);
        assert_eq!(pool.insert(player(1)).unwrap(), 0);
        assert_eq!(pool.insert(player(2)).unwrap(), 1);
        assert!(matches!(pool.insert(player(3)), Err(CoreError::PoolFull)));
        assert_eq!(pool.remove(0).unwrap().entity_id, 1);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.insert(player(3)).unwrap(), 0);
        assert_eq!(
            pool.iter().map(|p| p.entity_id).collect::<Vec<_>>(),
            vec![3, 2]
        );
    }

    #[test]
    fn removing_invalid_or_empty_slot_returns_entity_dead() {
        let mut pool = PlayerPool::new(1);
        assert!(matches!(pool.remove(0), Err(CoreError::EntityDead)));
        assert!(matches!(pool.remove(1), Err(CoreError::EntityDead)));
    }
}
