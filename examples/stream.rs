use futures::StreamExt;
use nowhear::{MediaEvent, MediaWatcher, MediaWatcherBuilder, Result};

#[tokio::main]
async fn main() -> Result<()> {
    // Create a media watcher
    let watcher = MediaWatcherBuilder::new().build().await?;

    println!("Watching for media events... (Press Ctrl+C to exit)");

    // Create an event stream
    let mut stream = watcher.event_stream().await?;

    // Process events as they arrive
    while let Some(event) = stream.next().await {
        match event {
            MediaEvent::TrackChanged { player_name, track } => {
                println!("\n🎵 Track changed on {player_name}");
                println!("   Title: {}", track.title);
                println!("   Artist: {}", track.artist.join(", "));
                if let Some(album) = track.album {
                    println!("   Album: {album}");
                }
            }
            MediaEvent::StateChanged { player_name, state } => {
                println!("\n▶️  Playback state changed on {player_name}: {state:?}");
            }
            MediaEvent::PositionChanged {
                player_name,
                position,
            } => {
                println!("\n⏩ Position changed on {player_name}: {position:?}");
            }
            MediaEvent::VolumeChanged {
                player_name,
                volume,
            } => {
                println!(
                    "\n🔊 Volume changed on {}: {:.0}%",
                    player_name,
                    volume * 100.0
                );
            }
            MediaEvent::PlayerAdded { player_name } => {
                println!("\n➕ Player added: {player_name}");
            }
            MediaEvent::PlayerRemoved { player_name } => {
                println!("\n➖ Player removed: {player_name}",);
            }
        }
    }

    Ok(())
}
