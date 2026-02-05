# Rusted Asteroids

A multiplayer TUI (Terminal User Interface) Asteroids game written in Rust. Battle asteroids and other players in a shared arena with power-ups, combos, and progressive waves.

## Features

- **Multiplayer** - Play with friends in a shared game room
- **Power-ups** - Collect Shield, Rapid Fire, Triple Shot, and Speed Boost
- **Combo System** - Chain kills for score multipliers (up to 10x)
- **Wave System** - Progressive difficulty with increasing asteroid counts
- **Kill Streaks** - Earn bonus points for consecutive player kills
- **PvP Combat** - Shoot other players for points
- **In-game Chat** - Communicate with other players

## Installation

### From Releases

Download the latest release for your platform from the [Releases](../../releases) page:

| Platform | File |
|----------|------|
| Linux x86_64 | `rusted-asteroids-linux-x86_64.tar.gz` |
| Linux ARM64 | `rusted-asteroids-linux-aarch64.tar.gz` |
| macOS Intel | `rusted-asteroids-macos-x86_64.tar.gz` |
| macOS Apple Silicon | `rusted-asteroids-macos-aarch64.tar.gz` |
| Windows | `rusted-asteroids-windows-x86_64.zip` |

### From Source

Requires [Rust](https://rustup.rs/) 1.70 or later.

```bash
git clone https://github.com/0xGingi/rusted-asteroids.git
cd rusted-asteroids
cargo build --release
```

Binaries will be in `target/release/`.

## Quick Start

### Join the Public Server

```bash
./client
```

You'll be prompted to enter your name, then connected to the public server.

### Host Your Own Server

```bash
# Start server (default port 4000)
./server

# In another terminal, connect locally
./client --addr=127.0.0.1:4000
```

## Usage

### Client

```bash
# Connect to default server with name prompt
./client

# Specify name directly
./client --name=YourName

# Connect to specific server
./client --addr=192.168.1.100:4000 --name=YourName

# Using environment variable
ASTEROIDS_ADDR=192.168.1.100:4000 ./client
```

### Server

```bash
# Start on default port (4000)
./server

# Specify address and port
./server --addr=0.0.0.0:4000

# Specify port only
./server --port=4000

# Using environment variable
ASTEROIDS_ADDR=0.0.0.0:4000 ./server
```

## Controls

| Key | Action |
|-----|--------|
| `W` / `Up Arrow` | Thrust forward |
| `A` / `Left Arrow` | Rotate left |
| `D` / `Right Arrow` | Rotate right |
| `Space` | Fire |
| `C` | Enter chat mode |
| `Esc` | Exit chat mode |
| `Enter` | Send chat message |
| `Q` | Quit game |

## Gameplay

### Scoring

| Action | Points |
|--------|--------|
| Destroy small asteroid | 100 x combo |
| Destroy medium asteroid | 50 x combo |
| Destroy large asteroid | 20 x combo |
| Kill another player | 200 |
| Kill streak bonus (every 3 kills) | +100 |
| Death penalty | -15% of score |

### Power-ups

Power-ups spawn when asteroids are destroyed (30% chance) and last 8 seconds when collected.

| Power-up | Symbol | Color | Effect |
|----------|--------|-------|--------|
| Shield | `S` | Cyan | Invincibility |
| Rapid Fire | `R` | Red | 60% faster firing |
| Triple Shot | `T` | Magenta | Fire 3 bullets at once |
| Speed Boost | `B` | Blue | 50% faster movement |

### Combo System

Kill asteroids quickly to build your combo multiplier:
- Kills within 3 seconds increase your combo (max 10x)
- Your score is multiplied by your current combo
- Combo resets after 3 seconds of no kills or on death

### Wave System

- Game starts at Wave 1 with 50 asteroids
- When all asteroids are destroyed, a 3-second countdown begins
- Each new wave adds 5 more asteroids (max 100)
- Difficulty increases as waves progress

### Respawning

- When you die, there's a 1.5-second respawn delay
- You spawn in a safe location away from asteroids
- 2.5 seconds of spawn invincibility (player blinks)
- Screen border flashes red on death

## HUD Elements

The scoreboard shows:
- Current wave and asteroids remaining
- Your score and combo multiplier
- Active power-up effects (S R T B I)
- Kill streak count
- Respawn timer (when dead)
- Leaderboard (top 5 players)

## Development

```bash
# Run server in development
cargo run -p server

# Run client in development
cargo run -p client -- --name=Dev --addr=127.0.0.1:4000

# Run tests
cargo test

# Build release binaries
cargo build --release
```

## Architecture

```
rusted-asteroids/
├── shared/     # Common types and protocol (PlayerState, ServerMsg, etc.)
├── server/     # Game server (tick loop, collision, game logic)
├── client/     # TUI client (ratatui, input handling, rendering)
└── .github/    # CI/CD workflows
```

The game uses a simple TCP protocol with JSON-encoded messages. The server runs at 20 ticks per second and broadcasts game state to all connected clients.