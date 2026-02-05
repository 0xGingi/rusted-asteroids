use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use rand::Rng;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use shared::{
    AsteroidState, BulletState, ClientMsg, PlayerInput, PlayerState, PowerUpKind, PowerUpState,
    ServerMsg, WaveInfo,
};
use shared::{Vec2, WORLD_HEIGHT, WORLD_WIDTH};

struct InputState {
    thrust: bool,
    rotate_left: bool,
    rotate_right: bool,
    fire: bool,
    // Per-key timestamps for timeout (needed on macOS which lacks key release events)
    thrust_at: Option<Instant>,
    rotate_left_at: Option<Instant>,
    rotate_right_at: Option<Instant>,
    fire_at: Option<Instant>,
}

const INPUT_TIMEOUT_MS: u64 = 120;

impl Default for InputState {
    fn default() -> Self {
        Self {
            thrust: false,
            rotate_left: false,
            rotate_right: false,
            fire: false,
            thrust_at: None,
            rotate_left_at: None,
            rotate_right_at: None,
            fire_at: None,
        }
    }
}

impl InputState {
    fn clear(&mut self) {
        self.thrust = false;
        self.rotate_left = false;
        self.rotate_right = false;
        self.fire = false;
        self.thrust_at = None;
        self.rotate_left_at = None;
        self.rotate_right_at = None;
        self.fire_at = None;
    }

    fn check_timeout(&mut self) {
        let timeout = Duration::from_millis(INPUT_TIMEOUT_MS);
        let now = Instant::now();

        if let Some(t) = self.thrust_at {
            if now.duration_since(t) > timeout {
                self.thrust = false;
                self.thrust_at = None;
            }
        }
        if let Some(t) = self.rotate_left_at {
            if now.duration_since(t) > timeout {
                self.rotate_left = false;
                self.rotate_left_at = None;
            }
        }
        if let Some(t) = self.rotate_right_at {
            if now.duration_since(t) > timeout {
                self.rotate_right = false;
                self.rotate_right_at = None;
            }
        }
        if let Some(t) = self.fire_at {
            if now.duration_since(t) > timeout {
                self.fire = false;
                self.fire_at = None;
            }
        }
    }
}

enum Mode {
    Game,
    Chat,
}

struct ClientState {
    id: Option<u64>,
    name: String,
    players: HashMap<u64, PlayerState>,
    asteroids: Vec<AsteroidState>,
    bullets: Vec<BulletState>,
    power_ups: Vec<PowerUpState>,
    wave: Option<WaveInfo>,
    chat: Vec<String>,
    input: InputState,
    mode: Mode,
    chat_input: String,
    should_quit: bool,
    death_flash_until: Option<Instant>,
    last_alive: bool,
}

impl ClientState {
    fn new(name: String) -> Self {
        Self {
            id: None,
            name,
            players: HashMap::new(),
            asteroids: Vec::new(),
            bullets: Vec::new(),
            power_ups: Vec::new(),
            wave: None,
            chat: Vec::new(),
            input: InputState::default(),
            mode: Mode::Game,
            chat_input: String::new(),
            should_quit: false,
            death_flash_until: None,
            last_alive: true,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (addr, name) = parse_args();

    let stream = TcpStream::connect(&addr).await?;
    let (read_half, mut write_half) = stream.into_split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<ServerMsg>();

    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if let Ok(line) = serde_json::to_string(&msg) {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if write_half.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(msg) = serde_json::from_str::<ServerMsg>(&line) {
                let _ = in_tx.send(msg);
            }
        }
    });

    out_tx.send(ClientMsg::Join { name: name.clone() })?;

    let mut tui = Tui::new()?;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    tokio::task::spawn_blocking(move || loop {
        if event::poll(Duration::from_millis(5)).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                let _ = event_tx.send(ev);
            }
        }
    });

    let mut state = ClientState::new(name);
    let mut render_tick = tokio::time::interval(Duration::from_millis(33));
    let mut input_tick = tokio::time::interval(Duration::from_millis(16));

    loop {
        tokio::select! {
            Some(msg) = in_rx.recv() => {
                handle_server_msg(&mut state, msg);
            }
            Some(ev) = event_rx.recv() => {
                handle_event(&mut state, ev, &out_tx)?;
            }
            _ = input_tick.tick() => {
                if let Mode::Game = state.mode {
                    let input_msg = build_input(&mut state.input);
                    let _ = out_tx.send(ClientMsg::Input(input_msg));
                } else {
                    let _ = out_tx.send(ClientMsg::Input(PlayerInput::default()));
                }
            }
            _ = render_tick.tick() => {
                tui.draw(&state)?;
            }
        }

        if state.should_quit {
            break;
        }
    }

    Ok(())
}

fn build_input(input: &mut InputState) -> PlayerInput {
    // On platforms without Release events (macOS), auto-clear after timeout
    input.check_timeout();

    let rotate = if input.rotate_left && !input.rotate_right {
        -1
    } else if input.rotate_right && !input.rotate_left {
        1
    } else {
        0
    };

    PlayerInput {
        thrust: input.thrust,
        rotate,
        fire: input.fire,
    }
}

fn handle_server_msg(state: &mut ClientState, msg: ServerMsg) {
    match msg {
        ServerMsg::Welcome { id, .. } => {
            state.id = Some(id);
            state.chat.push(format!("connected as id {id}"));
        }
        ServerMsg::State {
            players,
            asteroids,
            bullets,
            power_ups,
            wave,
        } => {
            // Check if local player just died (was alive, now dead)
            if let Some(id) = state.id {
                let was_alive = state.last_alive;
                let now_alive = players.iter().find(|p| p.id == id).map_or(false, |p| p.alive);
                if was_alive && !now_alive {
                    state.death_flash_until = Some(Instant::now() + Duration::from_millis(500));
                }
                state.last_alive = now_alive;
            }

            state.players = players.into_iter().map(|p| (p.id, p)).collect();
            state.asteroids = asteroids;
            state.bullets = bullets;
            state.power_ups = power_ups;
            state.wave = wave;
        }
        ServerMsg::Chat { from, text } => {
            state.chat.push(format!("{from}: {text}"));
        }
        ServerMsg::System { text } => {
            state.chat.push(format!("* {text}"));
        }
        ServerMsg::Pong { .. } => {}
    }
    if state.chat.len() > 200 {
        let extra = state.chat.len() - 200;
        state.chat.drain(0..extra);
    }
}

fn handle_event(state: &mut ClientState, ev: Event, out_tx: &mpsc::UnboundedSender<ClientMsg>) -> Result<()> {
    match ev {
        Event::Key(key) => match state.mode {
            Mode::Chat => handle_chat_key(state, key, out_tx)?,
            Mode::Game => handle_game_key(state, key)?,
        },
        _ => {}
    }
    Ok(())
}

fn handle_chat_key(
    state: &mut ClientState,
    key: crossterm::event::KeyEvent,
    out_tx: &mpsc::UnboundedSender<ClientMsg>,
) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            state.mode = Mode::Game;
            state.chat_input.clear();
            state.input.clear();
        }
        KeyCode::Enter => {
            let text = state.chat_input.trim().to_string();
            if !text.is_empty() {
                let _ = out_tx.send(ClientMsg::Chat { text });
            }
            state.chat_input.clear();
            state.mode = Mode::Game;
            state.input.clear();
        }
        KeyCode::Backspace => {
            state.chat_input.pop();
        }
        KeyCode::Char(c) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                state.chat_input.push(c);
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_game_key(state: &mut ClientState, key: crossterm::event::KeyEvent) -> Result<()> {
    if key.code == KeyCode::Char('q') {
        state.should_quit = true;
        return Ok(());
    }

    if key.code == KeyCode::Char('c') {
        state.mode = Mode::Chat;
        state.input.clear();
        return Ok(());
    }

    let now = Instant::now();
    match key.code {
        KeyCode::Char('w') | KeyCode::Up => {
            if key.kind == KeyEventKind::Release {
                state.input.thrust = false;
                state.input.thrust_at = None;
            } else {
                state.input.thrust = true;
                state.input.thrust_at = Some(now);
            }
        }
        KeyCode::Char('a') | KeyCode::Left => {
            if key.kind == KeyEventKind::Release {
                state.input.rotate_left = false;
                state.input.rotate_left_at = None;
            } else {
                state.input.rotate_left = true;
                state.input.rotate_left_at = Some(now);
            }
        }
        KeyCode::Char('d') | KeyCode::Right => {
            if key.kind == KeyEventKind::Release {
                state.input.rotate_right = false;
                state.input.rotate_right_at = None;
            } else {
                state.input.rotate_right = true;
                state.input.rotate_right_at = Some(now);
            }
        }
        KeyCode::Char(' ') => {
            if key.kind == KeyEventKind::Release {
                state.input.fire = false;
                state.input.fire_at = None;
            } else {
                state.input.fire = true;
                state.input.fire_at = Some(now);
            }
        }
        _ => {}
    }
    Ok(())
}

struct Tui {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    style: Style,
}

impl Tui {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    fn draw(&mut self, state: &ClientState) -> Result<()> {
        self.terminal.draw(|f| {
            let size = f.size();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(10), Constraint::Length(7)])
                .split(size);

            let top = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(20), Constraint::Length(24)])
                .split(chunks[0]);

            // Death flash: red border when recently died
            let border_style = if state
                .death_flash_until
                .map_or(false, |t| Instant::now() < t)
            {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };

            let game_block = Block::default()
                .borders(Borders::ALL)
                .title("Asteroids")
                .border_style(border_style);
            let inner = game_block.inner(top[0]);
            let lines = build_world_lines(inner, state);
            let game = Paragraph::new(lines).block(game_block);
            f.render_widget(game, top[0]);

            let scoreboard = render_scoreboard(state);
            f.render_widget(scoreboard, top[1]);

            let chat = render_chat(chunks[1], state);
            f.render_widget(chat, chunks[1]);
        })?;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

fn build_world_lines(area: Rect, state: &ClientState) -> Vec<Line<'static>> {
    let w = area.width.max(1) as usize;
    let h = area.height.max(1) as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }

    let mut grid = vec![vec![Cell { ch: ' ', style: Style::default() }; w]; h];

    let center = if let Some(id) = state.id {
        state.players.get(&id).map(|p| p.pos).unwrap_or(Vec2::new(WORLD_WIDTH / 2.0, WORLD_HEIGHT / 2.0))
    } else {
        Vec2::new(WORLD_WIDTH / 2.0, WORLD_HEIGHT / 2.0)
    };

    let set_cell = |grid: &mut Vec<Vec<Cell>>, x: usize, y: usize, ch: char, style: Style| {
        if let Some(row) = grid.get_mut(y) {
            if let Some(cell) = row.get_mut(x) {
                cell.ch = ch;
                cell.style = style;
            }
        }
    };

    for ast in &state.asteroids {
        if let Some((x, y)) = world_to_view(ast.pos, center, area) {
            let style = Style::default().fg(Color::Yellow);
            match ast.size {
                1 => {
                    set_cell(&mut grid, x, y, 'o', style);
                }
                2 => {
                    set_cell(&mut grid, x, y, 'O', style);
                    if x > 0 { set_cell(&mut grid, x - 1, y, 'O', style); }
                    set_cell(&mut grid, x + 1, y, 'O', style);
                }
                _ => {
                    // Large asteroid: 3x2 block
                    for dy in 0..=1 {
                        for dx_off in -1i32..=1 {
                            let nx = (x as i32 + dx_off) as usize;
                            let ny = y + dy;
                            set_cell(&mut grid, nx, ny, '#', style);
                        }
                    }
                }
            };
        }
    }

    for bullet in &state.bullets {
        if let Some((x, y)) = world_to_view(bullet.pos, center, area) {
            set_cell(&mut grid, x, y, '*', Style::default().fg(Color::Red));
        }
    }

    // Render power-ups
    for pu in &state.power_ups {
        if let Some((x, y)) = world_to_view(pu.pos, center, area) {
            let (ch, color) = match pu.kind {
                PowerUpKind::Shield => ('S', Color::Cyan),
                PowerUpKind::RapidFire => ('R', Color::Red),
                PowerUpKind::TripleShot => ('T', Color::Magenta),
                PowerUpKind::SpeedBoost => ('B', Color::Blue),
            };
            set_cell(&mut grid, x, y, ch, Style::default().fg(color));
        }
    }

    let self_id = state.id;
    // Use milliseconds for blinking effect
    let blink_on = (Instant::now().elapsed().as_millis() / 150) % 2 == 0;

    for player in state.players.values() {
        if !player.alive {
            continue;
        }
        if let Some((x, y)) = world_to_view(player.pos, center, area) {
            let ch = if Some(player.id) == self_id {
                heading_glyph(player.angle)
            } else {
                'A'
            };

            // Check if player is invincible (has invincible_remaining or shield_remaining)
            let is_invincible = player.effects.invincible_remaining.is_some()
                || player.effects.shield_remaining.is_some();

            let style = if Some(player.id) == self_id {
                if is_invincible && !blink_on {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::Green)
                }
            } else if is_invincible && !blink_on {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Cyan)
            };
            set_cell(&mut grid, x, y, ch, style);
        }
    }

    grid_to_lines(grid)
}

fn shortest_delta(a: f32, b: f32, wrap: f32) -> f32 {
    let mut d = a - b;
    if d > wrap / 2.0 {
        d -= wrap;
    } else if d < -wrap / 2.0 {
        d += wrap;
    }
    d
}

const VIEW_ZOOM: f32 = 1.3; // Higher = more zoomed in, objects appear bigger

fn world_to_view(pos: Vec2, center: Vec2, area: Rect) -> Option<(usize, usize)> {
    if area.width < 2 || area.height < 2 {
        return None;
    }
    // Zoomed view shows less of the world
    let view_w = area.width as f32 / VIEW_ZOOM;
    let view_h = area.height as f32 / VIEW_ZOOM;
    let dx = shortest_delta(pos.x, center.x, WORLD_WIDTH);
    let dy = shortest_delta(pos.y, center.y, WORLD_HEIGHT);

    if dx.abs() > view_w / 2.0 || dy.abs() > view_h / 2.0 {
        return None;
    }

    let fx = (dx + view_w / 2.0) * ((area.width as f32 - 1.0) / view_w);
    let fy = (dy + view_h / 2.0) * ((area.height as f32 - 1.0) / view_h);
    let x = fx.round().clamp(0.0, area.width as f32 - 1.0) as usize;
    let y = fy.round().clamp(0.0, area.height as f32 - 1.0) as usize;
    Some((x, y))
}

fn heading_glyph(angle: f32) -> char {
    let mut a = angle % std::f32::consts::TAU;
    if a < 0.0 {
        a += std::f32::consts::TAU;
    }
    let sector = ((a + std::f32::consts::FRAC_PI_8) / std::f32::consts::FRAC_PI_4).floor() as i32;
    match sector.rem_euclid(8) {
        0 => '→',
        1 => '↘',
        2 => '↓',
        3 => '↙',
        4 => '←',
        5 => '↖',
        6 => '↑',
        _ => '↗',
    }
}

fn grid_to_lines(grid: Vec<Vec<Cell>>) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(grid.len());
    for row in grid {
        if row.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_style = row[0].style;
        let mut buf = String::new();
        for cell in row {
            if cell.style == current_style {
                buf.push(cell.ch);
            } else {
                spans.push(Span::styled(buf.clone(), current_style));
                buf.clear();
                buf.push(cell.ch);
                current_style = cell.style;
            }
        }
        spans.push(Span::styled(buf, current_style));
        lines.push(Line::from(spans));
    }
    lines
}

fn render_scoreboard(state: &ClientState) -> Paragraph<'static> {
    let mut lines = Vec::new();

    // Wave info
    if let Some(ref wave) = state.wave {
        let wave_text = if let Some(countdown) = wave.countdown {
            format!("Wave {} - Next: {:.1}s", wave.wave_number, countdown)
        } else {
            format!("Wave {} - {} left", wave.wave_number, wave.asteroids_remaining)
        };
        lines.push(Line::from(Span::styled(
            wave_text,
            Style::default().fg(Color::Magenta),
        )));
    }

    // Player's own status
    if let Some(id) = state.id {
        if let Some(player) = state.players.get(&id) {
            // Score and combo
            let combo_str = if player.combo > 1 {
                format!(" x{}", player.combo)
            } else {
                String::new()
            };
            lines.push(Line::from(format!("Score: {}{}", player.score, combo_str)));

            // Kill streak
            if player.kill_streak > 0 {
                lines.push(Line::from(Span::styled(
                    format!("Streak: {}", player.kill_streak),
                    Style::default().fg(Color::Red),
                )));
            }

            // Active effects
            let mut effects = Vec::new();
            if player.effects.shield_remaining.is_some() {
                effects.push(Span::styled("S", Style::default().fg(Color::Cyan)));
            }
            if player.effects.rapid_fire_remaining.is_some() {
                effects.push(Span::styled("R", Style::default().fg(Color::Red)));
            }
            if player.effects.triple_shot_remaining.is_some() {
                effects.push(Span::styled("T", Style::default().fg(Color::Magenta)));
            }
            if player.effects.speed_boost_remaining.is_some() {
                effects.push(Span::styled("B", Style::default().fg(Color::Blue)));
            }
            if player.effects.invincible_remaining.is_some() {
                effects.push(Span::styled("I", Style::default().fg(Color::White)));
            }
            if !effects.is_empty() {
                let mut spans = vec![Span::raw("Effects: ")];
                for (i, e) in effects.into_iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::raw(" "));
                    }
                    spans.push(e);
                }
                lines.push(Line::from(spans));
            }

            // Respawn timer
            if let Some(timer) = player.respawn_timer {
                lines.push(Line::from(Span::styled(
                    format!("Respawn: {:.1}s", timer),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Leaderboard",
        Style::default().fg(Color::Yellow),
    )));

    let mut players = state.players.values().cloned().collect::<Vec<_>>();
    players.sort_by_key(|p| std::cmp::Reverse(p.score));

    for p in players.into_iter().take(5) {
        let marker = if Some(p.id) == state.id { ">" } else { " " };
        let status = if !p.alive { " [dead]" } else { "" };
        lines.push(Line::from(format!(
            "{marker}{} ({}){status}",
            truncate_name(&p.name, 8),
            p.score
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("wasd/arrows space"));
    lines.push(Line::from("c:chat q:quit"));

    Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Info"))
        .wrap(Wrap { trim: true })
}

fn truncate_name(name: &str, max_len: usize) -> String {
    if name.len() <= max_len {
        name.to_string()
    } else {
        format!("{}…", &name[..max_len - 1])
    }
}

fn render_chat(area: Rect, state: &ClientState) -> Paragraph<'static> {
    let block = Block::default().borders(Borders::ALL).title("Chat");
    let inner = block.inner(area);
    let max_lines = inner.height.saturating_sub(1) as usize;
    let start = state.chat.len().saturating_sub(max_lines);

    let mut lines = state.chat[start..]
        .iter()
        .map(|l| Line::from(l.clone()))
        .collect::<Vec<_>>();

    let prompt = match state.mode {
        Mode::Chat => format!("> {}", state.chat_input),
        Mode::Game => "> (press c to chat)".to_string(),
    };
    lines.push(Line::from(prompt));

    Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
}

fn parse_args() -> (String, String) {
    let mut addr = "149.56.242.231:4000".to_string();
    let mut name: Option<String> = None;

    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--addr=") {
            addr = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--name=") {
            name = Some(v.to_string());
        }
    }

    if let Ok(v) = std::env::var("ASTEROIDS_ADDR") {
        addr = v;
    }

    let name = name.unwrap_or_else(|| prompt_for_name());

    (addr, name)
}

fn prompt_for_name() -> String {
    use std::io::Write;

    print!("Enter your name: ");
    std::io::stdout().flush().unwrap();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    let trimmed = input.trim();

    if trimmed.is_empty() {
        let mut rng = rand::thread_rng();
        format!("Player{}", rng.gen_range(1000..9999))
    } else {
        trimmed.to_string()
    }
}
