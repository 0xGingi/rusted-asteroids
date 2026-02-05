use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rand::Rng;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};

use shared::{
    wrap_position, AsteroidState, BulletState, ClientMsg, PlayerEffects, PlayerInput, PlayerState,
    PowerUpKind, PowerUpState, ServerMsg, Vec2, WaveInfo, WORLD_HEIGHT, WORLD_WIDTH,
};

const TICK_HZ: u32 = 20;
const ROT_SPEED: f32 = 5.0;
const THRUST: f32 = 12.0;
const DRAG: f32 = 0.985;
const MAX_SPEED: f32 = 25.0;
const BULLET_SPEED: f32 = 30.0;
const FIRE_COOLDOWN: f32 = 0.2;
const BULLET_TTL: f32 = 2.5;
const ASTEROID_COUNT: usize = 50;

// New gameplay constants
const SPAWN_INVINCIBILITY_SECS: f32 = 2.5;
const RESPAWN_DELAY_SECS: f32 = 1.5;
const SAFE_SPAWN_RADIUS: f32 = 8.0;
const POWERUP_SPAWN_CHANCE: f32 = 0.3;
const POWERUP_DURATION_SECS: f32 = 8.0;
const POWERUP_TTL_SECS: f32 = 15.0;
const POWERUP_RADIUS: f32 = 1.5;
const COMBO_TIMEOUT_SECS: f32 = 3.0;
const MAX_COMBO: u32 = 10;
const KILL_STREAK_BONUS_INTERVAL: u32 = 3;
const KILL_STREAK_BONUS_POINTS: u32 = 100;
const DEATH_PENALTY_PERCENT: f32 = 0.15;
const RAPID_FIRE_COOLDOWN_MULT: f32 = 0.4;
const SPEED_BOOST_MULT: f32 = 1.5;
const WAVE_COUNTDOWN_SECS: f32 = 3.0;
const ASTEROIDS_PER_WAVE: usize = 5;

// Collision radii
const PLAYER_RADIUS: f32 = 1.5;
const BULLET_RADIUS: f32 = 0.5;

fn asteroid_radius(size: u8) -> f32 {
    match size {
        1 => 2.0,
        2 => 3.0,
        _ => 4.0,
    }
}

fn shortest_delta(a: f32, b: f32, wrap: f32) -> f32 {
    let d = a - b;
    if d > wrap / 2.0 {
        d - wrap
    } else if d < -wrap / 2.0 {
        d + wrap
    } else {
        d
    }
}

fn distance_squared_wrapped(a: Vec2, b: Vec2) -> f32 {
    let dx = shortest_delta(a.x, b.x, WORLD_WIDTH);
    let dy = shortest_delta(a.y, b.y, WORLD_HEIGHT);
    dx * dx + dy * dy
}

#[derive(Clone)]
struct PlayerRuntime {
    input: PlayerInput,
    last_fire: Instant,
    // New fields for gameplay features
    invincible_until: Option<Instant>,
    respawn_at: Option<Instant>,
    last_kill_time: Option<Instant>,
    combo: u32,
    kill_streak: u32,
    shield_until: Option<Instant>,
    rapid_fire_until: Option<Instant>,
    triple_shot_until: Option<Instant>,
    speed_boost_until: Option<Instant>,
}

impl PlayerRuntime {
    fn new() -> Self {
        Self {
            input: PlayerInput::default(),
            last_fire: Instant::now(),
            invincible_until: None,
            respawn_at: None,
            last_kill_time: None,
            combo: 0,
            kill_streak: 0,
            shield_until: None,
            rapid_fire_until: None,
            triple_shot_until: None,
            speed_boost_until: None,
        }
    }

    fn is_invincible(&self) -> bool {
        self.invincible_until.map_or(false, |t| Instant::now() < t)
            || self.shield_until.map_or(false, |t| Instant::now() < t)
    }

    fn has_rapid_fire(&self) -> bool {
        self.rapid_fire_until.map_or(false, |t| Instant::now() < t)
    }

    fn has_triple_shot(&self) -> bool {
        self.triple_shot_until.map_or(false, |t| Instant::now() < t)
    }

    fn has_speed_boost(&self) -> bool {
        self.speed_boost_until.map_or(false, |t| Instant::now() < t)
    }

    fn get_effects(&self) -> PlayerEffects {
        let now = Instant::now();
        PlayerEffects {
            shield_remaining: self.shield_until.and_then(|t| {
                if t > now {
                    Some(t.duration_since(now).as_secs_f32())
                } else {
                    None
                }
            }),
            rapid_fire_remaining: self.rapid_fire_until.and_then(|t| {
                if t > now {
                    Some(t.duration_since(now).as_secs_f32())
                } else {
                    None
                }
            }),
            triple_shot_remaining: self.triple_shot_until.and_then(|t| {
                if t > now {
                    Some(t.duration_since(now).as_secs_f32())
                } else {
                    None
                }
            }),
            speed_boost_remaining: self.speed_boost_until.and_then(|t| {
                if t > now {
                    Some(t.duration_since(now).as_secs_f32())
                } else {
                    None
                }
            }),
            invincible_remaining: self.invincible_until.and_then(|t| {
                if t > now {
                    Some(t.duration_since(now).as_secs_f32())
                } else {
                    None
                }
            }),
        }
    }
}

struct PowerUpRuntime {
    state: PowerUpState,
    expires_at: Instant,
}

struct BulletRuntime {
    state: BulletState,
    ttl: f32,
}

type ClientTx = mpsc::UnboundedSender<ServerMsg>;

struct ServerState {
    next_id: u64,
    players: HashMap<u64, PlayerState>,
    runtime: HashMap<u64, PlayerRuntime>,
    bullets: Vec<BulletRuntime>,
    asteroids: Vec<AsteroidState>,
    clients: HashMap<u64, ClientTx>,
    // New fields for gameplay features
    power_ups: Vec<PowerUpRuntime>,
    current_wave: u32,
    wave_countdown: Option<Instant>,
}

impl ServerState {
    fn new() -> Self {
        let mut s = Self {
            next_id: 1,
            players: HashMap::new(),
            runtime: HashMap::new(),
            bullets: Vec::new(),
            asteroids: Vec::new(),
            clients: HashMap::new(),
            power_ups: Vec::new(),
            current_wave: 1,
            wave_countdown: None,
        };
        s.asteroids = spawn_asteroids(ASTEROID_COUNT, &mut s);
        s
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let addr = parse_addr();
    let listener = TcpListener::bind(&addr).await?;
    println!("server listening on {addr}");

    let state = Arc::new(Mutex::new(ServerState::new()));
    let tick_state = Arc::clone(&state);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(1000 / TICK_HZ as u64));
        loop {
            ticker.tick().await;
            let (players, asteroids, bullets, power_ups, wave, clients) = {
                let mut s = tick_state.lock().await;
                tick(&mut s, 1.0 / TICK_HZ as f32);
                let players = s.players.values().cloned().collect::<Vec<_>>();
                let asteroids = s.asteroids.clone();
                let bullets = s
                    .bullets
                    .iter()
                    .map(|b| b.state.clone())
                    .collect::<Vec<_>>();
                let power_ups = s
                    .power_ups
                    .iter()
                    .map(|p| p.state.clone())
                    .collect::<Vec<_>>();
                let wave = Some(WaveInfo {
                    wave_number: s.current_wave,
                    asteroids_remaining: s.asteroids.len() as u32,
                    countdown: s.wave_countdown.map(|t| {
                        let now = Instant::now();
                        if t > now {
                            t.duration_since(now).as_secs_f32()
                        } else {
                            0.0
                        }
                    }),
                });
                let clients = s.clients.values().cloned().collect::<Vec<_>>();
                (players, asteroids, bullets, power_ups, wave, clients)
            };

            let msg = ServerMsg::State {
                players,
                asteroids,
                bullets,
                power_ups,
                wave,
            };
            broadcast(&clients, msg);
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, state).await {
                eprintln!("client error: {err:?}");
            }
        });
    }
}

async fn handle_client(stream: TcpStream, state: Arc<Mutex<ServerState>>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();

    let (id, tick_hz) = {
        let mut s = state.lock().await;
        let id = s.next_id;
        s.next_id += 1;
        s.clients.insert(id, tx.clone());
        (id, TICK_HZ)
    };

    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let line = serde_json::to_string(&msg).unwrap_or_default();
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if write_half.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let _ = tx.send(ServerMsg::Welcome { id, tick_hz });
    let _ = tx.send(ServerMsg::System {
        text: "Welcome to rusted-asteroids".to_string(),
    });

    while let Some(line) = reader.next_line().await? {
        let msg: ClientMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        match msg {
            ClientMsg::Join { name } => {
                let sys_msg = {
                    let mut s = state.lock().await;
                    if !s.players.contains_key(&id) {
                        let player = spawn_player(id, name.clone(), &s.asteroids);
                        s.players.insert(id, player);
                        let mut rt = PlayerRuntime::new();
                        // Give spawn invincibility
                        rt.invincible_until =
                            Some(Instant::now() + Duration::from_secs_f32(SPAWN_INVINCIBILITY_SECS));
                        s.runtime.insert(id, rt);
                    }
                    format!("{name} joined the room")
                };
                broadcast_all(&state, ServerMsg::System { text: sys_msg }).await;
            }
            ClientMsg::Input(input) => {
                let mut s = state.lock().await;
                if let Some(rt) = s.runtime.get_mut(&id) {
                    rt.input = input;
                }
            }
            ClientMsg::Chat { text } => {
                let name = {
                    let s = state.lock().await;
                    s.players
                        .get(&id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| format!("Player{id}"))
                };
                broadcast_all(
                    &state,
                    ServerMsg::Chat {
                        from: name,
                        text,
                    },
                )
                .await;
            }
            ClientMsg::Ping { nonce } => {
                let _ = tx.send(ServerMsg::Pong { nonce });
            }
        }
    }

    write_task.abort();
    disconnect(id, state).await;
    Ok(())
}

async fn disconnect(id: u64, state: Arc<Mutex<ServerState>>) {
    let name = {
        let mut s = state.lock().await;
        s.clients.remove(&id);
        s.runtime.remove(&id);
        s.players.remove(&id).map(|p| p.name)
    };

    if let Some(name) = name {
        broadcast_all(&state, ServerMsg::System { text: format!("{name} left the room") }).await;
    }
}

fn tick(s: &mut ServerState, dt: f32) {
    let mut rng = rand::thread_rng();
    let now = Instant::now();

    // Process respawn timers first
    let mut players_to_respawn: Vec<u64> = Vec::new();
    for (id, rt) in s.runtime.iter_mut() {
        if let Some(respawn_at) = rt.respawn_at {
            if now >= respawn_at {
                players_to_respawn.push(*id);
                rt.respawn_at = None;
            }
        }
    }

    // Respawn players whose timer expired
    for id in players_to_respawn {
        if let Some(player) = s.players.get_mut(&id) {
            let safe_pos = find_safe_spawn_position(&s.asteroids);
            player.pos = safe_pos;
            player.vel = Vec2::new(0.0, 0.0);
            player.alive = true;
            player.respawn_timer = None;
            if let Some(rt) = s.runtime.get_mut(&id) {
                rt.invincible_until =
                    Some(now + Duration::from_secs_f32(SPAWN_INVINCIBILITY_SECS));
            }
        }
    }

    // Update player state from runtime (effects, combo, streak)
    for (id, player) in s.players.iter_mut() {
        if let Some(rt) = s.runtime.get(id) {
            player.effects = rt.get_effects();
            player.combo = rt.combo;
            player.kill_streak = rt.kill_streak;
            if let Some(respawn_at) = rt.respawn_at {
                if respawn_at > now {
                    player.respawn_timer = Some(respawn_at.duration_since(now).as_secs_f32());
                }
            }
        }
    }

    // Player movement and shooting
    let player_ids: Vec<u64> = s.players.keys().cloned().collect();
    for id in player_ids {
        let (_player_pos, _player_angle, player_alive, input, has_rapid, has_triple, has_speed, last_fire) = {
            let player = match s.players.get(&id) {
                Some(p) => p,
                None => continue,
            };
            let rt = match s.runtime.get(&id) {
                Some(r) => r,
                None => continue,
            };
            (
                player.pos,
                player.angle,
                player.alive,
                rt.input.clone(),
                rt.has_rapid_fire(),
                rt.has_triple_shot(),
                rt.has_speed_boost(),
                rt.last_fire,
            )
        };

        if !player_alive {
            continue;
        }

        // Update angle and velocity
        let player = s.players.get_mut(&id).unwrap();
        player.angle += input.rotate as f32 * ROT_SPEED * dt;

        let thrust_mult = if has_speed { SPEED_BOOST_MULT } else { 1.0 };
        if input.thrust {
            let dir = Vec2::new(player.angle.cos(), player.angle.sin());
            player.vel = player.vel.add(dir.scale(THRUST * thrust_mult * dt));
        }

        player.vel = player.vel.scale(DRAG);
        let max_speed = if has_speed {
            MAX_SPEED * SPEED_BOOST_MULT
        } else {
            MAX_SPEED
        };
        let speed_sq = player.vel.x * player.vel.x + player.vel.y * player.vel.y;
        if speed_sq > max_speed * max_speed {
            let scale = max_speed / speed_sq.sqrt();
            player.vel = player.vel.scale(scale);
        }
        player.pos = wrap_position(player.pos.add(player.vel.scale(dt)));

        // Shooting
        if input.fire {
            let cooldown = if has_rapid {
                FIRE_COOLDOWN * RAPID_FIRE_COOLDOWN_MULT
            } else {
                FIRE_COOLDOWN
            };
            let elapsed = last_fire.elapsed().as_secs_f32();
            if elapsed >= cooldown {
                if let Some(rt) = s.runtime.get_mut(&id) {
                    rt.last_fire = Instant::now();
                }

                // Snap angle to 8 directions
                let mut a = player.angle % std::f32::consts::TAU;
                if a < 0.0 {
                    a += std::f32::consts::TAU;
                }
                let sector =
                    ((a + std::f32::consts::FRAC_PI_8) / std::f32::consts::FRAC_PI_4).floor();
                let snapped_angle = sector * std::f32::consts::FRAC_PI_4;

                // Create bullets (1 or 3 depending on triple shot)
                let angles = if has_triple {
                    vec![
                        snapped_angle - 0.2,
                        snapped_angle,
                        snapped_angle + 0.2,
                    ]
                } else {
                    vec![snapped_angle]
                };

                for angle in angles {
                    let dir = Vec2::new(angle.cos(), angle.sin());
                    let bullet_id = s.next_id;
                    s.next_id += 1;
                    s.bullets.push(BulletRuntime {
                        state: BulletState {
                            id: bullet_id,
                            owner_id: id,
                            pos: player.pos,
                            vel: dir.scale(BULLET_SPEED),
                        },
                        ttl: BULLET_TTL,
                    });
                }
            }
        }
    }

    // Update bullets
    for bullet in &mut s.bullets {
        bullet.ttl -= dt;
        bullet.state.pos = bullet.state.pos.add(bullet.state.vel.scale(dt));
    }
    s.bullets.retain(|b| {
        b.ttl > 0.0
            && b.state.pos.x >= 0.0
            && b.state.pos.x <= WORLD_WIDTH
            && b.state.pos.y >= 0.0
            && b.state.pos.y <= WORLD_HEIGHT
    });

    // Update asteroids
    for ast in &mut s.asteroids {
        ast.pos = wrap_position(ast.pos.add(ast.vel.scale(dt)));
    }

    // Update power-ups (remove expired)
    s.power_ups.retain(|p| now < p.expires_at);

    // Collision: bullet-asteroid
    let mut bullets_to_remove: Vec<u64> = Vec::new();
    let mut asteroids_to_remove: Vec<u64> = Vec::new();
    let mut new_asteroids: Vec<AsteroidState> = Vec::new();
    let mut asteroid_kills: Vec<(u64, u64, Vec2, Vec2)> = Vec::new(); // (bullet_owner, asteroid_id, pos, vel)

    for bullet in &s.bullets {
        for ast in &s.asteroids {
            let dist_sq = distance_squared_wrapped(bullet.state.pos, ast.pos);
            let radius_sum = BULLET_RADIUS + asteroid_radius(ast.size);
            if dist_sq < radius_sum * radius_sum {
                bullets_to_remove.push(bullet.state.id);
                asteroids_to_remove.push(ast.id);
                asteroid_kills.push((bullet.state.owner_id, ast.id, ast.pos, ast.vel));

                // Award points with combo multiplier
                let base_points = match ast.size {
                    1 => 100,
                    2 => 50,
                    _ => 20,
                };

                if let Some(rt) = s.runtime.get_mut(&bullet.state.owner_id) {
                    // Check combo timing
                    let combo_active = rt
                        .last_kill_time
                        .map_or(false, |t| t.elapsed().as_secs_f32() < COMBO_TIMEOUT_SECS);

                    if combo_active {
                        rt.combo = (rt.combo + 1).min(MAX_COMBO);
                    } else {
                        rt.combo = 1;
                    }
                    rt.last_kill_time = Some(now);

                    let multiplier = rt.combo;
                    let points = base_points * multiplier;

                    if let Some(player) = s.players.get_mut(&bullet.state.owner_id) {
                        player.score += points;
                        player.combo = rt.combo;
                    }
                }

                // Split asteroid with velocity inheritance
                if ast.size > 1 {
                    let new_size = ast.size - 1;
                    for i in 0..2 {
                        let spread_angle = rng.gen_range(-0.5..0.5);
                        let parent_angle = ast.vel.y.atan2(ast.vel.x);
                        let new_angle = parent_angle
                            + spread_angle
                            + if i == 0 {
                                std::f32::consts::FRAC_PI_4
                            } else {
                                -std::f32::consts::FRAC_PI_4
                            };
                        let parent_speed =
                            (ast.vel.x * ast.vel.x + ast.vel.y * ast.vel.y).sqrt();
                        let new_speed = parent_speed * rng.gen_range(0.8..1.3) + 1.0;
                        let offset_angle = if i == 0 {
                            new_angle
                        } else {
                            new_angle + std::f32::consts::PI
                        };
                        new_asteroids.push(AsteroidState {
                            id: s.next_id,
                            pos: wrap_position(
                                ast.pos
                                    .add(Vec2::new(offset_angle.cos(), offset_angle.sin())),
                            ),
                            vel: Vec2::new(new_angle.cos() * new_speed, new_angle.sin() * new_speed),
                            size: new_size,
                        });
                        s.next_id += 1;
                    }
                }

                // Chance to spawn power-up
                if rng.gen::<f32>() < POWERUP_SPAWN_CHANCE {
                    let kind = match rng.gen_range(0..4) {
                        0 => PowerUpKind::Shield,
                        1 => PowerUpKind::RapidFire,
                        2 => PowerUpKind::TripleShot,
                        _ => PowerUpKind::SpeedBoost,
                    };
                    s.power_ups.push(PowerUpRuntime {
                        state: PowerUpState {
                            id: s.next_id,
                            pos: ast.pos,
                            kind,
                        },
                        expires_at: now + Duration::from_secs_f32(POWERUP_TTL_SECS),
                    });
                    s.next_id += 1;
                }

                break;
            }
        }
    }

    s.bullets.retain(|b| !bullets_to_remove.contains(&b.state.id));
    s.asteroids.retain(|a| !asteroids_to_remove.contains(&a.id));
    s.asteroids.extend(new_asteroids);

    // Collision: player-power-up
    let mut power_ups_to_remove: Vec<u64> = Vec::new();
    for player in s.players.values() {
        if !player.alive {
            continue;
        }
        for pu in &s.power_ups {
            let dist_sq = distance_squared_wrapped(player.pos, pu.state.pos);
            let radius_sum = PLAYER_RADIUS + POWERUP_RADIUS;
            if dist_sq < radius_sum * radius_sum {
                power_ups_to_remove.push(pu.state.id);
                if let Some(rt) = s.runtime.get_mut(&player.id) {
                    let effect_end = now + Duration::from_secs_f32(POWERUP_DURATION_SECS);
                    match pu.state.kind {
                        PowerUpKind::Shield => rt.shield_until = Some(effect_end),
                        PowerUpKind::RapidFire => rt.rapid_fire_until = Some(effect_end),
                        PowerUpKind::TripleShot => rt.triple_shot_until = Some(effect_end),
                        PowerUpKind::SpeedBoost => rt.speed_boost_until = Some(effect_end),
                    }
                }
            }
        }
    }
    s.power_ups
        .retain(|p| !power_ups_to_remove.contains(&p.state.id));

    // Collision: player-asteroid (check invincibility)
    let mut players_killed_by_asteroid: Vec<u64> = Vec::new();
    for player in s.players.values() {
        if !player.alive {
            continue;
        }
        let is_invincible = s
            .runtime
            .get(&player.id)
            .map_or(false, |rt| rt.is_invincible());
        if is_invincible {
            continue;
        }
        for ast in &s.asteroids {
            let dist_sq = distance_squared_wrapped(player.pos, ast.pos);
            let radius_sum = PLAYER_RADIUS + asteroid_radius(ast.size);
            if dist_sq < radius_sum * radius_sum {
                players_killed_by_asteroid.push(player.id);
                break;
            }
        }
    }

    // Apply asteroid deaths
    for id in players_killed_by_asteroid {
        apply_death(s, id, None);
    }

    // Collision: bullet-player (PvP, check invincibility)
    let mut player_kills: Vec<(u64, u64)> = Vec::new();
    let mut bullets_hit: Vec<u64> = Vec::new();
    for bullet in &s.bullets {
        for player in s.players.values() {
            if !player.alive || player.id == bullet.state.owner_id {
                continue;
            }
            let is_invincible = s
                .runtime
                .get(&player.id)
                .map_or(false, |rt| rt.is_invincible());
            if is_invincible {
                continue;
            }
            let dist_sq = distance_squared_wrapped(bullet.state.pos, player.pos);
            let radius_sum = BULLET_RADIUS + PLAYER_RADIUS;
            if dist_sq < radius_sum * radius_sum {
                bullets_hit.push(bullet.state.id);
                player_kills.push((player.id, bullet.state.owner_id));
                break;
            }
        }
    }

    // Apply player kills
    for (victim_id, shooter_id) in player_kills {
        apply_death(s, victim_id, Some(shooter_id));

        // Award kill streak
        if let Some(rt) = s.runtime.get_mut(&shooter_id) {
            rt.kill_streak += 1;
            let streak_bonus = if rt.kill_streak % KILL_STREAK_BONUS_INTERVAL == 0 {
                KILL_STREAK_BONUS_POINTS
            } else {
                0
            };
            if let Some(shooter) = s.players.get_mut(&shooter_id) {
                shooter.score += 200 + streak_bonus;
                shooter.kill_streak = rt.kill_streak;
            }
        }
    }
    s.bullets.retain(|b| !bullets_hit.contains(&b.state.id));

    // Wave system: check if all asteroids cleared
    if s.asteroids.is_empty() {
        if s.wave_countdown.is_none() {
            // Start countdown for next wave
            s.wave_countdown = Some(now + Duration::from_secs_f32(WAVE_COUNTDOWN_SECS));
        } else if let Some(countdown_end) = s.wave_countdown {
            if now >= countdown_end {
                // Spawn next wave
                s.current_wave += 1;
                let asteroid_count = ASTEROID_COUNT + (s.current_wave as usize - 1) * ASTEROIDS_PER_WAVE;
                s.asteroids = spawn_asteroids(asteroid_count.min(100), s);
                s.wave_countdown = None;
            }
        }
    }
}

fn apply_death(s: &mut ServerState, victim_id: u64, _killer_id: Option<u64>) {
    let now = Instant::now();

    if let Some(victim) = s.players.get_mut(&victim_id) {
        victim.alive = false;
        // Death penalty: lose 15% of score
        victim.score = ((victim.score as f32) * (1.0 - DEATH_PENALTY_PERCENT)) as u32;
        victim.respawn_timer = Some(RESPAWN_DELAY_SECS);
    }

    if let Some(rt) = s.runtime.get_mut(&victim_id) {
        rt.respawn_at = Some(now + Duration::from_secs_f32(RESPAWN_DELAY_SECS));
        rt.combo = 0;
        rt.kill_streak = 0;
        // Clear power-up effects on death
        rt.shield_until = None;
        rt.rapid_fire_until = None;
        rt.triple_shot_until = None;
        rt.speed_boost_until = None;
    }
}

fn find_safe_spawn_position(asteroids: &[AsteroidState]) -> Vec2 {
    let mut rng = rand::thread_rng();
    for _ in 0..50 {
        let pos = Vec2::new(
            rng.gen_range(0.0..WORLD_WIDTH),
            rng.gen_range(0.0..WORLD_HEIGHT),
        );
        let mut safe = true;
        for ast in asteroids {
            let dist_sq = distance_squared_wrapped(pos, ast.pos);
            let min_dist = SAFE_SPAWN_RADIUS + asteroid_radius(ast.size);
            if dist_sq < min_dist * min_dist {
                safe = false;
                break;
            }
        }
        if safe {
            return pos;
        }
    }
    // Fallback: just pick random position
    Vec2::new(
        rng.gen_range(0.0..WORLD_WIDTH),
        rng.gen_range(0.0..WORLD_HEIGHT),
    )
}

fn spawn_player(id: u64, name: String, asteroids: &[AsteroidState]) -> PlayerState {
    let mut rng = rand::thread_rng();
    let pos = find_safe_spawn_position(asteroids);
    PlayerState {
        id,
        name,
        pos,
        vel: Vec2::new(0.0, 0.0),
        angle: rng.gen_range(0.0..std::f32::consts::TAU),
        alive: true,
        score: 0,
        combo: 0,
        kill_streak: 0,
        respawn_timer: None,
        effects: PlayerEffects::default(),
    }
}

fn spawn_asteroids(count: usize, s: &mut ServerState) -> Vec<AsteroidState> {
    let mut rng = rand::thread_rng();
    (0..count)
        .map(|_| AsteroidState {
            id: next_entity_id(s),
            pos: Vec2::new(rng.gen_range(0.0..WORLD_WIDTH), rng.gen_range(0.0..WORLD_HEIGHT)),
            vel: Vec2::new(rng.gen_range(-2.5..2.5), rng.gen_range(-1.5..1.5)),
            size: rng.gen_range(1..=3),
        })
        .collect()
}

fn next_entity_id(s: &mut ServerState) -> u64 {
    let id = s.next_id;
    s.next_id += 1;
    id
}

async fn broadcast_all(state: &Arc<Mutex<ServerState>>, msg: ServerMsg) {
    let clients = {
        let s = state.lock().await;
        s.clients.values().cloned().collect::<Vec<_>>()
    };
    broadcast(&clients, msg);
}

fn broadcast(clients: &[ClientTx], msg: ServerMsg) {
    for tx in clients {
        let _ = tx.send(msg.clone());
    }
}

fn parse_addr() -> String {
    let mut addr = "0.0.0.0:4000".to_string();
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--addr=") {
            addr = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--port=") {
            addr = format!("0.0.0.0:{v}");
        }
    }
    if let Ok(v) = std::env::var("ASTEROIDS_ADDR") {
        addr = v;
    }
    addr
}
