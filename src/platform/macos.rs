// Allow unexpected_cfgs warning from objc crate macros (msg_send!, class!, sel!)
// This is caused by old version of objc crate and doesn't affect functionality
#![allow(unexpected_cfgs)]

#[cfg(target_os = "macos")]
use crate::error::{MediaWatcherError, Result};
use crate::types::{MediaEvent, PlaybackState, PlayerInfo, Track};
use crate::watcher::{EventStream, MediaWatcher};
use async_trait::async_trait;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use futures::stream::Stream;
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::mpsc;

pub struct MacOSMediaWatcher {
    // Empty struct - all state is managed globally for Objective-C callbacks
}

// グローバルな状態を保持（Objective-Cコールバックからアクセス）
static mut EVENT_SENDER: Option<Arc<Mutex<Option<mpsc::UnboundedSender<MediaEvent>>>>> = None;
static mut OBSERVER: Option<id> = None;

impl MacOSMediaWatcher {
    pub async fn new() -> Result<Self> {
        Ok(Self {})
    }

    /// AppleScriptを実行してトラック情報を取得
    async fn get_current_track_from_music_app(&self) -> Result<Option<Track>> {
        let script = r#"
            tell application "Music"
                if player state is playing or player state is paused then
                    set track_info to {name, artist, album, player state as string} of current track
                    return track_info
                end if
            end tell
        "#;

        tokio::task::spawn_blocking(move || {
            execute_applescript(script)
                .ok()
                .and_then(|output| parse_music_output(&output))
        })
        .await
        .map_err(|e| MediaWatcherError::InternalError(e.to_string()))
    }

    async fn get_current_track_from_spotify(&self) -> Result<Option<Track>> {
        let script = r#"
            tell application "Spotify"
                if player state is playing or player state is paused then
                    set track_info to {name of current track, artist of current track, album of current track, player state as string}
                    return track_info
                end if
            end tell
        "#;

        tokio::task::spawn_blocking(move || {
            execute_applescript(script)
                .ok()
                .and_then(|output| parse_music_output(&output))
        })
        .await
        .map_err(|e| MediaWatcherError::InternalError(e.to_string()))
    }

    /// NSDistributedNotificationCenterでイベントストリームを作成
    fn create_event_stream_impl() -> impl Stream<Item = MediaEvent> {
        let (tx, rx) = mpsc::unbounded_channel();

        let sender = Arc::new(Mutex::new(Some(tx)));

        // Objective-Cのオブザーバーを登録
        unsafe {
            EVENT_SENDER = Some(sender.clone());

            let pool = NSAutoreleasePool::new(nil);

            // カスタムObserverクラスを作成・登録
            let observer = create_notification_observer();

            // NSDistributedNotificationCenterを取得
            let notification_center: id =
                msg_send![class!(NSDistributedNotificationCenter), defaultCenter];

            // Music.app (iTunes) の通知を購読
            let itunes_notification = NSString::alloc(nil).init_str("com.apple.iTunes.playerInfo");
            let _: () = msg_send![
                notification_center,
                addObserver: observer
                selector: sel!(handleNotification:)
                name: itunes_notification
                object: nil
            ];

            // Spotifyの通知を購読
            let spotify_notification =
                NSString::alloc(nil).init_str("com.spotify.client.PlaybackStateChanged");
            let _: () = msg_send![
                notification_center,
                addObserver: observer
                selector: sel!(handleNotification:)
                name: spotify_notification
                object: nil
            ];

            // オブザーバーをグローバルに保存
            OBSERVER = Some(observer);

            let _: () = msg_send![pool, drain];
        }

        tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
    }
}

#[async_trait]
impl MediaWatcher for MacOSMediaWatcher {
    async fn list_players(&self) -> Result<Vec<String>> {
        let mut players = Vec::new();

        // Check if Music.app is running and playing
        if self
            .get_current_track_from_music_app()
            .await
            .ok()
            .flatten()
            .is_some()
        {
            players.push("Music".to_string());
        }

        // Check if Spotify is running and playing
        if self
            .get_current_track_from_spotify()
            .await
            .ok()
            .flatten()
            .is_some()
        {
            players.push("Spotify".to_string());
        }

        Ok(players)
    }

    async fn get_player(&self, player_name: &str) -> Result<PlayerInfo> {
        match player_name {
            "Music" => {
                if let Some(track) = self.get_current_track_from_music_app().await? {
                    Ok(PlayerInfo {
                        player_name: "Music".to_string(),
                        current_track: Some(track),
                        playback_state: PlaybackState::Playing,
                        position: None,
                        volume: None,
                    })
                } else {
                    Ok(PlayerInfo::empty("Music"))
                }
            }
            "Spotify" => {
                if let Some(track) = self.get_current_track_from_spotify().await? {
                    Ok(PlayerInfo {
                        player_name: "Spotify".to_string(),
                        current_track: Some(track),
                        playback_state: PlaybackState::Playing,
                        position: None,
                        volume: None,
                    })
                } else {
                    Ok(PlayerInfo::empty("Spotify"))
                }
            }
            _ => Err(MediaWatcherError::PlayerNotFound(player_name.to_string())),
        }
    }

    async fn event_stream(&self) -> Result<EventStream> {
        let stream = Self::create_event_stream_impl();
        Ok(Box::pin(stream))
    }
}

// ========== Helper Functions ==========

/// AppleScriptを実行して結果を取得
fn execute_applescript(script: &str) -> Result<String> {
    use std::process::Command;

    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| {
            MediaWatcherError::InternalError(format!("Failed to execute AppleScript: {}", e))
        })?;

    if !output.status.success() {
        return Err(MediaWatcherError::InternalError(format!(
            "AppleScript error: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// AppleScriptの出力をパース
fn parse_music_output(output: &str) -> Option<Track> {
    // AppleScriptの出力形式: "title, artist, album, state"
    let parts: Vec<&str> = output.split(", ").collect();

    if parts.len() >= 3 {
        Some(Track {
            title: parts[0].to_string(),
            artist: vec![parts[1].to_string()],
            album: if parts[2].is_empty() {
                None
            } else {
                Some(parts[2].to_string())
            },
            album_artist: None,
            track_number: None,
            duration: None,
            art_url: None,
        })
    } else {
        None
    }
}

/// Objective-Cの通知オブザーバークラスを作成
unsafe fn create_notification_observer() -> id {
    // 既に登録されているかチェック
    let class_name = "MediaWatcherNotificationObserver";
    if let Some(existing_class) = Class::get(class_name) {
        let observer: id = msg_send![existing_class, alloc];
        let observer: id = msg_send![observer, init];
        return observer;
    }

    // 新しいクラスを作成
    let superclass = class!(NSObject);
    let mut decl = ClassDecl::new(class_name, superclass).unwrap();

    // handleNotification: メソッドを追加
    extern "C" fn handle_notification(_this: &Object, _cmd: Sel, notification: id) {
        unsafe {
            let event_sender_ptr = &raw const EVENT_SENDER;
            if let Some(sender_ref) = &*event_sender_ptr
                && let Some(sender) = sender_ref.lock().unwrap().as_ref()
            {
                // 通知から情報を抽出
                let user_info: id = msg_send![notification, userInfo];

                if user_info != nil {
                    // iTunes/Music.appの場合
                    let name_key = NSString::alloc(nil).init_str("Name");
                    let artist_key = NSString::alloc(nil).init_str("Artist");
                    let album_key = NSString::alloc(nil).init_str("Album");

                    let name: id = msg_send![user_info, objectForKey: name_key];
                    let artist: id = msg_send![user_info, objectForKey: artist_key];
                    let album: id = msg_send![user_info, objectForKey: album_key];

                    if name != nil {
                        let track = Track {
                            title: nsstring_to_string(name),
                            artist: vec![nsstring_to_string(artist)],
                            album: if album != nil {
                                Some(nsstring_to_string(album))
                            } else {
                                None
                            },
                            album_artist: None,
                            track_number: None,
                            duration: None,
                            art_url: None,
                        };

                        let event = MediaEvent::TrackChanged {
                            player_name: "Music".to_string(),
                            track,
                        };

                        let _ = sender.send(event);
                    }
                }
            }
        }
    }

    unsafe {
        decl.add_method(
            sel!(handleNotification:),
            handle_notification as extern "C" fn(&Object, Sel, id),
        );
    }

    let class = decl.register();
    let observer: id = msg_send![class, alloc];
    let observer: id = msg_send![observer, init];
    observer
}

/// NSStringをRustのStringに変換
unsafe fn nsstring_to_string(nsstring: id) -> String {
    if nsstring == nil {
        return String::new();
    }

    let cstr: *const c_char = msg_send![nsstring, UTF8String];
    if cstr.is_null() {
        return String::new();
    }

    unsafe { CStr::from_ptr(cstr).to_string_lossy().into_owned() }
}
