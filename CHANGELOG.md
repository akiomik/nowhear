# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **BREAKING**: Changed `Track::album_artist` from `Option<Vec<String>>` to `Vec<String>` for API consistency with `Track::artist`
- **BREAKING**: macOS: Normalized volume range from 0-100 to 0.0-1.0 to match Linux implementation

## [0.1.3] - 2026-01-31

### Changed

- Linux: Migrated to async MPRIS API using native `zbus` implementation for improved performance and better async/await integration
- macOS/Windows: Changed polling interval behavior to skip missed ticks instead of bursting, improving performance under system load

## [0.1.2] - 2026-01-27

### Added

- macOS: Album artwork url support for Spotify

### Changed

- macOS: Switched from AppleScript to JXA (JavaScript for Automation) for improved performance

## [0.1.1] - 2026-01-14

### Added

- macOS: Duration support for Music.app and Spotify
- macOS: Album artist support for Music.app and Spotify
- macOS: Track number support for Music.app and Spotify

### Changed

- `MediaSource::get_player` now accepts `impl AsRef<str>` instead of `&str`, allowing more flexible string type usage (e.g., `String`, `&str`, `&String`, `Box<str>`)
- macOS: Extracted AppleScript code into separate `.applescript` files for better maintainability

## [0.1.0] - 2026-01-13

### Added

- Initial release of nowhear library
- Cross-platform support for Linux, macOS, and Windows
- `MediaSource` trait for unified API across platforms
- `MediaSourceBuilder` for creating platform-specific media sources
- Media player information retrieval (`list_players`, `get_player`)
- Event streaming for real-time media playback monitoring
- Support for the following media events:
  - `TrackChanged`: Fired when a new track starts playing
  - `StateChanged`: Fired when playback state changes
  - `PositionChanged`: Fired when playback position changes (seek)
  - `VolumeChanged`: Fired when volume changes
  - `PlayerAdded`: Fired when a new player becomes available
  - `PlayerRemoved`: Fired when a player becomes unavailable
- Rich track metadata including title, artist, album, duration, and artwork URL
- Platform-specific implementations:
  - Linux: MPRIS D-Bus interface support
  - macOS: AppleScript support for Music.app and Spotify
  - Windows: Windows Media Control API support
- Two example applications: `basic` and `stream`

[unreleased]: https://github.com/akiomik/nowhear/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/akiomik/nowhear/releases/tag/v0.1.3
[0.1.2]: https://github.com/akiomik/nowhear/releases/tag/v0.1.2
[0.1.1]: https://github.com/akiomik/nowhear/releases/tag/v0.1.1
[0.1.0]: https://github.com/akiomik/nowhear/releases/tag/v0.1.0
