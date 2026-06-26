//! In-process Open Scripting Architecture (OSA) execution for macOS.
//!
//! The previous macOS backend spawned a fresh `osascript` subprocess on every
//! poll. Profiling an embedding application showed that the `posix_spawn`
//! syscall alone accounted for ~17% of CPU samples — the spawn, not the Apple
//! Event round-trips, was the dominant cost.
//!
//! This module replaces the per-poll spawn with a single long-lived worker
//! thread that compiles the script once via `OSAKit` (`OSAScript`) and executes
//! it in-process on every request. Measured cost drops from ~71 ms/poll
//! (spawn) to a few ms/poll (in-process), with no subprocess to supervise.
//!
//! # Threading
//!
//! `OSAScript` instances have thread affinity, so a single dedicated thread
//! owns every compiled script and performs all executions. Callers (the async
//! polling task) hand work to it over a channel and await the reply, which
//! keeps OSA off the host application's main thread.
//!
//! All Objective-C objects stay on the worker thread; only plain Rust values
//! (`String`, `Result`) cross the channel boundary.

use std::collections::HashMap;
use std::ptr;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::thread;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use tokio::sync::oneshot;

use crate::error::{MediaSourceError, Result};

/// A unit of work for the OSA worker thread: a script to run and a channel to
/// deliver its string result back to the awaiting caller.
struct Request {
    source: &'static str,
    reply: oneshot::Sender<Result<String>>,
}

/// Lazily-initialized handle to the OSA worker thread.
static ENGINE: OnceLock<Sender<Request>> = OnceLock::new();

/// Executes `source` (a JavaScript-for-Automation script) in-process and
/// returns its string result.
///
/// The script is compiled once on first use and cached on the worker thread;
/// subsequent calls reuse the compiled form. `source` is `'static` because in
/// practice it is an `include_str!`-embedded script with a process lifetime.
pub async fn execute(source: &'static str) -> Result<String> {
    let tx = ENGINE.get_or_init(spawn_worker);

    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(Request {
        source,
        reply: reply_tx,
    })
    .map_err(|_| MediaSourceError::InternalError("OSA worker thread is not running".to_string()))?;

    reply_rx.await.map_err(|_| {
        MediaSourceError::InternalError("OSA worker dropped the request".to_string())
    })?
}

/// Spawns the dedicated worker thread and returns the request sender.
fn spawn_worker() -> Sender<Request> {
    let (tx, rx) = channel::<Request>();

    thread::Builder::new()
        .name("nowhear-osa".to_string())
        .spawn(move || {
            // Compiled scripts are cached by source pointer/identity so each
            // distinct script is compiled only once. In practice there is a
            // single embedded script.
            let mut scripts: HashMap<&'static str, OsaScript> = HashMap::new();

            while let Ok(Request { source, reply }) = rx.recv() {
                // Drain autoreleased objects (the result descriptor and its
                // string value) every iteration so the long-lived thread does
                // not accumulate memory.
                let result = autoreleasepool(|_| {
                    let script = match scripts.get(source) {
                        Some(script) => script,
                        None => match OsaScript::compile(source) {
                            Ok(compiled) => scripts.entry(source).or_insert(compiled),
                            // Compilation failures are not cached, so a transient
                            // failure can be retried on the next poll.
                            Err(e) => return Err(e),
                        },
                    };
                    script.execute()
                });

                // The receiver is gone if the poll was cancelled; that is fine.
                let _ = reply.send(result);
            }
        })
        .expect("failed to spawn nowhear OSA worker thread");

    tx
}

/// A compiled `OSAScript`, retained for the lifetime of the worker thread.
///
/// Not `Send`/`Sync`: it must only ever be touched on the worker thread.
struct OsaScript {
    script: Retained<AnyObject>,
}

impl OsaScript {
    /// Compiles `source` as a JavaScript OSA script.
    fn compile(source: &str) -> Result<Self> {
        autoreleasepool(|_| unsafe {
            // OSALanguage *js = [OSALanguage languageForName:@"JavaScript"];
            let lang_cls = class!(OSALanguage);
            let js_name = NSString::from_str("JavaScript");
            let js_lang: *mut AnyObject = msg_send![lang_cls, languageForName: &*js_name];
            if js_lang.is_null() {
                return Err(MediaSourceError::InternalError(
                    "JavaScript OSA language is unavailable".to_string(),
                ));
            }

            // OSAScript *s = [[OSAScript alloc] initWithSource:src language:js];
            let osa_cls = class!(OSAScript);
            let src = NSString::from_str(source);
            let alloc: *mut AnyObject = msg_send![osa_cls, alloc];
            let script: *mut AnyObject = msg_send![alloc, initWithSource: &*src, language: js_lang];
            // `init` returns a +1 reference; take ownership of it.
            let script = Retained::from_raw(script).ok_or_else(|| {
                MediaSourceError::InternalError("OSAScript initialization failed".to_string())
            })?;

            // Compile up front so execution failures are distinguishable from
            // syntax errors, and so the compiled form is cached.
            let mut err: *mut AnyObject = ptr::null_mut();
            let ok: bool = msg_send![&*script, compileAndReturnError: &mut err];
            if !ok {
                return Err(MediaSourceError::InternalError(format!(
                    "OSA script compilation failed: {}",
                    error_message(err)
                )));
            }

            Ok(Self { script })
        })
    }

    /// Executes the compiled script and returns its string result.
    fn execute(&self) -> Result<String> {
        unsafe {
            let mut err: *mut AnyObject = ptr::null_mut();
            // NSAppleEventDescriptor *desc = [s executeAndReturnError:&err];
            let desc: *mut AnyObject = msg_send![&*self.script, executeAndReturnError: &mut err];
            if desc.is_null() {
                return Err(MediaSourceError::InternalError(format!(
                    "OSA script execution failed: {}",
                    error_message(err)
                )));
            }

            let value: *mut AnyObject = msg_send![desc, stringValue];
            if value.is_null() {
                return Err(MediaSourceError::InternalError(
                    "OSA script returned no string value".to_string(),
                ));
            }

            let value: &NSString = &*value.cast();
            Ok(value.to_string())
        }
    }
}

/// Best-effort extraction of a human-readable message from an OSA error dict.
///
/// `err` is the `NSDictionary*` populated by `compileAndReturnError:` /
/// `executeAndReturnError:`. Returns a placeholder when no message is present.
unsafe fn error_message(err: *mut AnyObject) -> String {
    if err.is_null() {
        return "(no error information)".to_string();
    }

    let key = NSString::from_str("OSAScriptErrorMessage");
    let message: *mut AnyObject = unsafe { msg_send![err, objectForKey: &*key] };
    if message.is_null() {
        return "(unknown OSA error)".to_string();
    }

    let message: &NSString = unsafe { &*message.cast() };
    message.to_string()
}
