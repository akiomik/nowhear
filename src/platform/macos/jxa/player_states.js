// Queries both Music.app and Spotify in a single osascript invocation to
// halve the number of process spawns per poll. Each player is wrapped in its
// own try/catch so that a failure in one (e.g. the app is not installed) does
// not prevent the other from being reported, preserving the per-player error
// isolation that separate processes used to provide.
function getState(app, includeArtwork) {
	if (!app.running()) {
		return null;
	}

	const playerState = app.playerState();
	if (playerState !== "playing" && playerState !== "paused") {
		return null;
	}

	const state = {
		playerState,
		playerPosition: app.playerPosition(),
		soundVolume: app.soundVolume(),
		trackName: app.currentTrack.name(),
		trackArtist: app.currentTrack.artist(),
		trackAlbum: app.currentTrack.album(),
		trackAlbumArtist: app.currentTrack.albumArtist(),
		trackNumber: app.currentTrack.trackNumber(),
		trackDuration: app.currentTrack.duration(),
	};

	if (includeArtwork) {
		state.trackAlbumArtworkUrl = app.currentTrack.artworkUrl();
	}

	return state;
}

function safeGetState(appName, includeArtwork) {
	try {
		return getState(Application(appName), includeArtwork);
	} catch (_e) {
		return null;
	}
}

JSON.stringify({
	music: safeGetState("Music", false),
	spotify: safeGetState("Spotify", true),
});
