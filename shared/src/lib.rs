use serde::{Deserialize, Serialize};

pub const WORLD_WIDTH: f32 = 240.0;
pub const WORLD_HEIGHT: f32 = 80.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PowerUpKind {
    Shield,
    RapidFire,
    TripleShot,
    SpeedBoost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerUpState {
    pub id: u64,
    pub pos: Vec2,
    pub kind: PowerUpKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayerEffects {
    pub shield_remaining: Option<f32>,
    pub rapid_fire_remaining: Option<f32>,
    pub triple_shot_remaining: Option<f32>,
    pub speed_boost_remaining: Option<f32>,
    pub invincible_remaining: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WaveInfo {
    pub wave_number: u32,
    pub asteroids_remaining: u32,
    pub countdown: Option<f32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn add(self, other: Vec2) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }

    pub fn scale(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayerInput {
    pub thrust: bool,
    #[serde(default)]
    pub target_angle: Option<f32>,
    pub fire: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerState {
    pub id: u64,
    pub name: String,
    pub pos: Vec2,
    pub vel: Vec2,
    pub angle: f32,
    pub alive: bool,
    pub score: u32,
    #[serde(default)]
    pub combo: u32,
    #[serde(default)]
    pub kill_streak: u32,
    #[serde(default)]
    pub respawn_timer: Option<f32>,
    #[serde(default)]
    pub effects: PlayerEffects,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsteroidState {
    pub id: u64,
    pub pos: Vec2,
    pub vel: Vec2,
    pub size: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulletState {
    pub id: u64,
    pub owner_id: u64,
    pub pos: Vec2,
    pub vel: Vec2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    Join { name: String },
    Input(PlayerInput),
    Chat { text: String },
    Ping { nonce: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome { id: u64, tick_hz: u32 },
    State {
        players: Vec<PlayerState>,
        asteroids: Vec<AsteroidState>,
        bullets: Vec<BulletState>,
        #[serde(default)]
        power_ups: Vec<PowerUpState>,
        #[serde(default)]
        wave: Option<WaveInfo>,
    },
    Chat { from: String, text: String },
    System { text: String },
    Pong { nonce: u64 },
}

pub fn wrap_position(mut p: Vec2) -> Vec2 {
    if p.x < 0.0 {
        p.x += WORLD_WIDTH;
    }
    if p.x >= WORLD_WIDTH {
        p.x -= WORLD_WIDTH;
    }
    if p.y < 0.0 {
        p.y += WORLD_HEIGHT;
    }
    if p.y >= WORLD_HEIGHT {
        p.y -= WORLD_HEIGHT;
    }
    p
}
