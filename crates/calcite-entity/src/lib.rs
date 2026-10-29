use calcite_core::{CoreError, Uuid};

/// A three-dimensional position in world coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3<T> {
    pub x: T,
    pub y: T,
    pub z: T,
}

/// Position and orientation state shared by entity simulation and observers.
///
/// The presence flags allow movement packets or other partial updates to carry
/// only a position, only a rotation, or both without coupling this type to a
/// transport representation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PosRot {
    pub position: Vec3<f64>,
    pub pitch: f32,
    pub yaw: f32,
    pub head_yaw: f32,
    pub is_on_ground: bool,
    pub is_pushing_against_wall: bool,
    pub is_position_present: bool,
    pub is_rotation_present: bool,
}

impl Default for PosRot {
    fn default() -> Self {
        Self {
            position: Vec3::default(),
            pitch: 0.0,
            yaw: 0.0,
            head_yaw: 0.0,
            is_on_ground: false,
            is_pushing_against_wall: false,
            is_position_present: false,
            is_rotation_present: false,
        }
    }
}

impl PosRot {
    /// Applies the fields present in `other` while always refreshing movement flags.
    pub fn partial_update(&mut self, other: Self) {
        if other.is_position_present {
            self.position = other.position;
            self.is_position_present = true;
        }
        if other.is_rotation_present {
            self.pitch = other.pitch;
            self.yaw = other.yaw;
            self.head_yaw = other.head_yaw;
            self.is_rotation_present = true;
        }
        self.is_on_ground = other.is_on_ground;
        self.is_pushing_against_wall = other.is_pushing_against_wall;
    }
}

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

    #[test]
    fn partial_position_update_preserves_rotation() {
        let mut state = PosRot {
            position: Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            pitch: 10.0,
            yaw: 20.0,
            head_yaw: 30.0,
            is_rotation_present: true,
            ..PosRot::default()
        };
        state.partial_update(PosRot {
            position: Vec3 {
                x: 8.0,
                y: 9.0,
                z: 10.0,
            },
            is_position_present: true,
            is_on_ground: true,
            ..PosRot::default()
        });

        assert_eq!(
            state.position,
            Vec3 {
                x: 8.0,
                y: 9.0,
                z: 10.0
            }
        );
        assert_eq!((state.pitch, state.yaw, state.head_yaw), (10.0, 20.0, 30.0));
        assert!(state.is_position_present);
        assert!(state.is_rotation_present);
        assert!(state.is_on_ground);
    }

    #[test]
    fn partial_rotation_update_preserves_position_and_updates_flags() {
        let mut state = PosRot {
            position: Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            is_position_present: true,
            ..PosRot::default()
        };
        state.partial_update(PosRot {
            pitch: -15.0,
            yaw: 45.0,
            head_yaw: 60.0,
            is_rotation_present: true,
            is_pushing_against_wall: true,
            ..PosRot::default()
        });

        assert_eq!(
            state.position,
            Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0
            }
        );
        assert_eq!(
            (state.pitch, state.yaw, state.head_yaw),
            (-15.0, 45.0, 60.0)
        );
        assert!(state.is_position_present);
        assert!(state.is_rotation_present);
        assert!(state.is_pushing_against_wall);
    }

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
