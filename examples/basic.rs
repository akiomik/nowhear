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
