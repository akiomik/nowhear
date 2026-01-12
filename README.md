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
| **macOS** | AppleScript | Music.app, Spotify |
| **Windows** | Windows Media Control | Spotify, Windows Media Player, VLC, iTunes, Chrome/Edge, and any SMTC-compatible app |

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
nowhear = "0.1"
tokio = { version = "1.0", features = ["macros", "rt-multi-thread"] }
futures = "0.3"
```

## Usage

### Basic Example

```rust
use nowhear::{MediaWatcher, MediaWatcherBuilder, Result};

#[tokio::main]
async fn main() -> Result<()> {
    // Create a media watcher
    let watcher = MediaWatcherBuilder::new().build().await?;
    
    // List all active media players
    let players = watcher.list_players().await?;
    println!("Active players: {players:?}");
    
    // Get information for a specific player
    if let Some(player_name) = players.first() {
        let info = watcher.get_player(player_name).await?;
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
use nowhear::{MediaEvent, MediaWatcher, MediaWatcherBuilder, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let watcher = MediaWatcherBuilder::new().build().await?;
    
    // Create an event stream
    let mut stream = watcher.event_stream().await?;
    
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

### MediaWatcher Trait

```rust
#[async_trait]
pub trait MediaWatcher: Send + Sync {
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
    pub album_artist: Option<Vec<String>>,
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
- Polling interval: 500ms

### macOS

- Uses AppleScript to communicate with media players
- Does not launch media applications if they're not already running
- Supports Music.app and Spotify
- Polling interval: 1000ms

### Windows

- Uses the Windows Media Control API (Windows 10 version 1809 or later)
- Supports any application that integrates with System Media Transport Controls (SMTC)
- Volume information is not available through the Windows Media Control API
- Polling interval: 1000ms

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
