# nowhear

[![CI](https://github.com/akiomik/nowhear/actions/workflows/ci.yml/badge.svg)](https://github.com/akiomik/nowhear/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/nowhear.svg)](https://crates.io/crates/nowhear)
[![Documentation](https://docs.rs/nowhear/badge.svg)](https://docs.rs/nowhear)
[![License](https://img.shields.io/crates/l/nowhear.svg)](https://github.com/akiomik/nowhear/blob/main/LICENSE)
[![codecov](https://codecov.io/gh/akiomik/nowhear/graph/badge.svg?token=GU40650KX6)](https://codecov.io/gh/akiomik/nowhear)

Cross-platform library for monitoring media playback information in Rust.

## Features

- 🎵 **Get currently playing media information** across Linux, macOS, and Windows
- 📡 **Subscribe to media events** via async streams
- 🔄 **Unified API** across all platforms
- ⚡ **Async/await** support with Tokio
- 🦀 **Pure Rust** implementation

## Supported Platforms

| Platform | API | Supported Players |
|----------|-----|-------------------|
| **Linux** | MPRIS (D-Bus) | Spotify, VLC, Rhythmbox, Chromium, Firefox, and any MPRIS-compatible player |
| **macOS** | JXA (Open Scripting) | Music.app, Spotify |
| **Windows** | Windows Media Control | Spotify, Windows Media Player, VLC, iTunes, Chrome/Edge, and any SMTC-compatible app |

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
nowhear = "0.4"
tokio = { version = "1.0", features = ["macros", "rt-multi-thread"] }
futures = "0.3"
```

## Usage

### Basic Example

```rust
use nowhear::{MediaSource, MediaSourceBuilder, Result};

#[tokio::main]
async fn main() -> Result<()> {
    // Create a media source
    let source = MediaSourceBuilder::new().build().await?;

    // List all active media players
    let players = source.list_players().await?;
    println!("Active players: {players:?}");

    // Get information for a specific player
    if let Some(player_name) = players.first() {
        let info = source.get_player(player_name).await?;
        println!("Player: {}", info.player_name);
        println!("State: {:?}", info.playback_state);
        if let Some(track) = info.current_track {
            println!("Track: {} - {}", track.artist.join(", "), track.title);
        }
    }

    Ok(())
}
```

### Event Stream Example

```rust
use futures::StreamExt;
use nowhear::{MediaEvent, MediaSource, MediaSourceBuilder, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let source = MediaSourceBuilder::new().build().await?;

    // Create an event stream
    let mut stream = source.event_stream().await?;

    // Process events as they arrive
    while let Some(event) = stream.next().await {
        match event {
            MediaEvent::TrackChanged { player_name, track } => {
                println!("🎵 {} - {}", track.artist.join(", "), track.title);
            }
            MediaEvent::StateChanged { player_name, state } => {
                println!("▶️  Playback state: {:?}", state);
            }
            MediaEvent::PlayerAdded { player_name } => {
                println!("➕ Player added: {}", player_name);
            }
            MediaEvent::PlayerRemoved { player_name } => {
                println!("➖ Player removed: {}", player_name);
            }
            _ => {}
        }
    }

    Ok(())
}
```

## API Overview

### MediaSource Trait

```rust
pub trait MediaSource: Send + Sync {
    /// List all available players
    async fn list_players(&self) -> Result<Vec<String>>;

    /// Get information for a specific player
    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo>;

    /// Create an event stream that yields media events
    async fn event_stream(&self) -> Result<EventStream>;
}
```

### Events

```rust
pub enum MediaEvent {
    /// A new track started playing
    TrackChanged { player_name: String, track: Track },

    /// Playback state changed (playing, paused, stopped)
    StateChanged { player_name: String, state: PlaybackState },

    /// Playback position changed (seek)
    PositionChanged { player_name: String, position: Duration },

    /// Volume changed
    VolumeChanged { player_name: String, volume: f64 },

    /// A new player appeared
    PlayerAdded { player_name: String },

    /// A player disappeared
    PlayerRemoved { player_name: String },
}
```

### Track Information

```rust
pub struct Track {
    pub title: String,
    pub artist: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Vec<String>,
    pub track_number: Option<u32>,
    pub duration: Option<Duration>,
    pub art_url: Option<String>,
}
```

## Examples

Run the included examples:

```bash
# Basic usage - list players and get current playback info
cargo run --example basic

# Stream events - monitor media events in real-time
cargo run --example stream
```

## Platform-Specific Notes

### Linux

- Requires D-Bus and MPRIS-compatible media players
- Works out of the box on most modern Linux distributions
- Event-driven architecture using native D-Bus signals for real-time updates
- **Build requirements**: `libdbus-1-dev` and `pkg-config` packages are required
  ```bash
  # Debian/Ubuntu
  sudo apt-get install libdbus-1-dev pkg-config

  # Fedora/RHEL
  sudo dnf install dbus-devel pkgconf-pkg-config

  # Arch Linux
  sudo pacman -S dbus pkg-config
  ```

### macOS

- Uses JavaScript for Automation (JXA), executed in-process via the Open Scripting Architecture (OSAKit), to query media players
- Requires macOS 10.10 Yosemite or later (JXA support required)
- Sends Apple Events to the media applications, so the host application may prompt for Automation permission on first use (System Settings → Privacy & Security → Automation)
- Does not launch media applications if they're not already running
- Supports Music.app and Spotify
- Polling interval: 1000ms

### Windows

- Uses the Windows Media Control API (Windows 10 version 1809 or later)
- Supports any application that integrates with System Media Transport Controls (SMTC)
- Event-driven architecture using Windows Runtime event handlers for real-time updates
- Volume information is not available through the Windows Media Control API

## Development

Run build for all platforms:

```bash
just build
```

Run build for a specific platform:

```bash
# Linux
just build-linux

# macOS
just build-macos

# Windows
just build-windows
```
