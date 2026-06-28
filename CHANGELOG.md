# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `MediaSourceError::PermissionDenied` variant. On macOS, `get_player` now returns this instead of `InternalError` when the host application lacks Automation permission (the underlying Apple Event fails with `errAEEventNotPermitted`), so callers can detect the permission case and prompt the user. Note: adding an enum variant can break downstream code that matches `MediaSourceError` exhaustively without a wildcard arm
- Optional `serde` feature: derives `Serialize` and `Deserialize` for the public data types (`Track`, `Artwork`, `PlaybackState`, `PlayerInfo`, and `MediaEvent`). `Duration` fields (`Track::duration`, `PlayerInfo::position`, and `MediaEvent::PositionChanged::position`) are represented as integer milliseconds; `Artwork` and `MediaEvent` are internally tagged with a `"type"` field; and `Artwork::Bytes` image data is base64-encoded
- `Artwork` enum modeling album artwork as either a URI (`Artwork::Url`, used by Linux and macOS) or raw image bytes (`Artwork::Bytes`, used by Windows). It provides `as_url`, `as_bytes`, `mime`, and an infallible `to_uri` that returns a directly renderable string (the URL as-is, or a freshly built `data:<mime>;base64,…` URI). The bytes are held in an `Arc<[u8]>` so cloning a `Track` does not copy the image

### Changed

- **BREAKING**: `Track::art_url: Option<String>` is replaced by `Track::artwork: Option<Artwork>`. Previously the Windows backend eagerly base64-encoded every thumbnail into a `data:` URI string; it now carries the raw bytes and only encodes on demand via `Artwork::to_uri`. Consumers that relied on the artwork always being a string should call `Artwork::to_uri()`
- **BREAKING**: `MediaSourceError` is now `#[non_exhaustive]`, so downstream `match` expressions on it must include a wildcard (`_`) arm. This is a one-time break that lets new variants be added in future releases without further breaking changes

## [0.4.1] - 2026-06-28

### Fixed

- Windows: `Track::art_url` is now populated. The thumbnail stream returned by Windows Media Control is read and inlined as a `data:<mime>;base64,<data>` URI; previously the code attempted to stringify the stream reference, which always failed and left `art_url` as `None`

## [0.4.0] - 2026-06-27

### Changed

- macOS: Run the player-state query in-process via OSAKit (`OSAScript`) on a dedicated worker thread instead of spawning an `osascript` subprocess on every poll. Profiling an embedding application showed the per-poll `posix_spawn` was the dominant CPU cost; this eliminates it (measured ~2.7x faster per poll while playing, and no subprocess is spawned during polling). The existing `player_states.js` is reused unchanged
- macOS: The Automation (Apple Events) permission is now requested by the host application rather than `osascript`, since scripts run in-process. After upgrading, users may see a one-time Automation prompt for the host application
- macOS: Fetch Music.app track fields with a single `currentTrack.properties()` Apple Event instead of one round-trip per field, roughly halving the per-poll cost while Music is playing. Spotify continues to read fields individually because it does not support `properties` on a track
- macOS: Determine which players are running in-process via `NSRunningApplication` instead of an Apple Event `running` check inside the script. This removes the per-poll `LaunchServices` lookups (a significant share of the remaining CPU while playing), and skips OSA execution entirely when neither Music.app nor Spotify is running

## [0.3.1] - 2026-06-27

### Added

- Optional `tracing` feature: enables diagnostic instrumentation via the `tracing` crate. Emits `debug`- and `warn`-level events for background task lifecycle, silently skipped errors, and platform-specific failures (D-Bus parse errors on Linux, AppleScript errors on macOS, Windows Media Control session errors on Windows)

### Changed

- macOS: Query Music.app and Spotify with a single `osascript` invocation instead of one per player, halving the number of process spawns per poll. Player state retrieval is now backed by a single consolidated JXA script (`player_states.js`)

## [0.3.0] - 2026-06-21

### Changed

- Windows: Migrated from polling-based to fully event-driven architecture using Windows Runtime event handlers (`SessionsChanged`, `MediaPropertiesChanged`, `PlaybackInfoChanged`, `TimelinePropertiesChanged`) for real-time updates with improved performance and reduced resource usage
- Windows: Deduplicated session enumeration into a shared helper and register session handlers before reading state so each newly discovered session is queried only once, reducing redundant Windows Media Control API calls
- Windows/macOS: Unified state-change detection into a single shared `diff_player_state` helper, so both backends report events identically
- **BREAKING**: macOS: `TrackChanged` is now emitted only when the track title or artist changes. Metadata-only updates (album, artwork, duration, track number) no longer emit `TrackChanged`, matching the Windows backend
- **BREAKING**: macOS: A track change now also emits a `PositionChanged` for the new track's position (the seek baseline resets on track change), instead of suppressing it when the new position happens to be within 2 seconds of the previous track's position

### Fixed

- Linux: A `PropertiesChanged` or `Seeked` signal from a player missing from the internal name cache no longer terminates the entire event stream. Such signals can race ahead of the `NameOwnerChanged` that populates the cache, or trail a player's removal; they are now skipped instead of propagating a fatal error
- Linux: The event-stream background task now shuts down promptly when the consumer drops the stream, and no longer leaks (the task, its D-Bus match rules, and its connection handle) when an idle player sends no further signals. A send-failure inside the `PropertiesChanged` handler now tears down the task instead of leaving the loop spinning
- Linux: A single undecodable or malformed D-Bus signal no longer terminates the entire event stream. Per-message stream and parse errors are now skipped so monitoring continues

## [0.2.0] - 2026-02-03

### Changed

- **BREAKING**: Changed `Track::album_artist` from `Option<Vec<String>>` to `Vec<String>` for API consistency with `Track::artist`
- **BREAKING**: Empty strings are now filtered out from `Track::artist` and `Track::album_artist` arrays
- **BREAKING**: `Track::album` and `Track::art_url` now return `None` instead of `Some("")` when values are empty
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

[unreleased]: https://github.com/akiomik/nowhear/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/akiomik/nowhear/releases/tag/v0.4.1
[0.4.0]: https://github.com/akiomik/nowhear/releases/tag/v0.4.0
[0.3.1]: https://github.com/akiomik/nowhear/releases/tag/v0.3.1
[0.3.0]: https://github.com/akiomik/nowhear/releases/tag/v0.3.0
[0.2.0]: https://github.com/akiomik/nowhear/releases/tag/v0.2.0
[0.1.3]: https://github.com/akiomik/nowhear/releases/tag/v0.1.3
[0.1.2]: https://github.com/akiomik/nowhear/releases/tag/v0.1.2
[0.1.1]: https://github.com/akiomik/nowhear/releases/tag/v0.1.1
[0.1.0]: https://github.com/akiomik/nowhear/releases/tag/v0.1.0
