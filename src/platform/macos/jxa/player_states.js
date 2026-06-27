// Helper definitions for querying Music.app and Spotify. The caller (Rust)
// appends the entry point — a `JSON.stringify({ music, spotify })` that invokes
// `safeGetState` only for players it has already confirmed are running, so this
// script never calls `app.running()` itself. Each query is wrapped in its own
// try/catch so a failure in one player does not prevent the other from being
// reported.

// Reads the current track's fields with one Apple Event via `properties()`.
//
// Reading each field individually (`currentTrack.name()`, `.artist()`, ...)
// costs one Apple Event round-trip per field; `properties()` returns them all
// at once, roughly halving the per-poll cost while a track is playing. Used for
// Music.app, which supports it. (Spotify does not — see `readTrackIndividually`.)
// biome-ignore lint/correctness/noUnusedVariables: called by JXA caller appended at runtime
function readTrackProperties(app) {
	const track = app.currentTrack.properties();
	return {
		trackName: track.name,
		trackArtist: track.artist,
		trackAlbum: track.album,
		trackAlbumArtist: track.albumArtist,
		trackNumber: track.trackNumber,
		trackDuration: track.duration,
	};
}

// Reads the current track's fields one property at a time.
//
// Spotify does not support `properties` on a track — requesting it raises an
// AppleEvent handler error (-10000) — so its fields must be read individually.
// biome-ignore lint/correctness/noUnusedVariables: called by JXA caller appended at runtime
function readTrackIndividually(app) {
	const track = app.currentTrack;
	return {
		trackName: track.name(),
		trackArtist: track.artist(),
		trackAlbum: track.album(),
		trackAlbumArtist: track.albumArtist(),
		trackNumber: track.trackNumber(),
		trackDuration: track.duration(),
	};
}

function getState(app, readTrack, includeArtwork) {
	const playerState = app.playerState();
	if (playerState !== "playing" && playerState !== "paused") {
		return null;
	}

	const state = Object.assign(
		{
			playerState,
			playerPosition: app.playerPosition(),
			soundVolume: app.soundVolume(),
		},
		readTrack(app),
	);

	if (includeArtwork) {
		state.trackAlbumArtworkUrl = app.currentTrack.artworkUrl();
	}

	return state;
}

// biome-ignore lint/correctness/noUnusedVariables: called by JXA caller appended at runtime
function safeGetState(appName, readTrack, includeArtwork) {
	try {
		return getState(Application(appName), readTrack, includeArtwork);
	} catch (_e) {
		return null;
	}
}
