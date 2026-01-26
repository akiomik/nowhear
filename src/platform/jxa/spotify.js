const app = Application("Spotify");
if (app.running()) {
	const playerState = app.playerState();
	if (playerState === "playing" || playerState === "paused") {
		JSON.stringify({
			playerState,
			playerPosition: app.playerPosition(),
			soundVolume: app.soundVolume(),
			trackName: app.currentTrack.name(),
			trackArtist: app.currentTrack.artist(),
			trackAlbum: app.currentTrack.album(),
			trackAlbumArtist: app.currentTrack.albumArtist(),
			trackAlbumArtworkUrl: app.currentTrack.artworkUrl(),
			trackNumber: app.currentTrack.trackNumber(),
			trackDuration: app.currentTrack.duration(),
		});
	}
}
