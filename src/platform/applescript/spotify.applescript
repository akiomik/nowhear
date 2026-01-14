if application "Spotify" is running then
  tell application "Spotify"
    if player state is playing or player state is paused then
      set trackName to name of current track
      set trackArtist to artist of current track
      set trackAlbum to album of current track
      set trackAlbumArtist to album artist of current track
      set trackNum to track number of current track
      set playerState to player state as string
      set playerPos to player position as string
      set soundVol to sound volume as string
      set trackDur to duration of current track
      return trackName & (ASCII character 9) & trackArtist & (ASCII character 9) & trackAlbum & (ASCII character 9) & trackAlbumArtist & (ASCII character 9) & trackNum & (ASCII character 9) & playerState & (ASCII character 9) & playerPos & (ASCII character 9) & soundVol & (ASCII character 9) & trackDur
    end if
  end tell
end if
return ""
