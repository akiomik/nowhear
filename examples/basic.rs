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
