//! Stand-in for the npm `ws` package that ircHub.js and eventsub.js use (no JS counterpart file).
//! One task per socket reads and writes (so ping replies are always flushed), reports `WsEvent`s to a
//! callback in order, and ends after its terminal event (`Close` or `Error`).
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone)]
pub enum WsEvent {
    Open,
    Message(String),
    /// close code (None when the peer sent none) and reason
    Close(Option<u16>, String),
    Error(String),
}

enum Out {
    Text(String),
    Close,
}

pub struct WsHandle {
    tx: UnboundedSender<Out>,
    task: JoinHandle<()>,
    closing: std::sync::atomic::AtomicBool,
}

impl WsHandle {
    /// `ws.send`: false when the socket is already gone.
    pub fn send(&self, text: &str) -> bool {
        self.tx.send(Out::Text(text.to_string())).is_ok()
    }

    /// `ws.close()`: a close frame, then the task ends without reporting anything further.
    pub fn close(&self) {
        self.closing.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.tx.send(Out::Close);
    }

    /// `ws.terminate()`: drop the connection now.
    pub fn terminate(&self) {
        self.task.abort();
    }
}

impl Drop for WsHandle {
    fn drop(&mut self) {
        // A socket that was asked to close is left to finish its close handshake.
        if !self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            self.task.abort();
        }
    }
}

/// Connects in the background; `on_event` is called from the socket's task.
pub fn connect(url: String, user_agent: String, on_event: impl Fn(WsEvent) + Send + 'static) -> WsHandle {
    let (tx, mut rx) = unbounded_channel::<Out>();
    let task = tokio::spawn(async move {
        let mut req = match url.as_str().into_client_request() {
            Ok(r) => r,
            Err(e) => return on_event(WsEvent::Error(e.to_string())),
        };
        if let Ok(v) = user_agent.parse() {
            req.headers_mut().insert("User-Agent", v);
        }
        let mut ws = match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => ws,
            Err(e) => return on_event(WsEvent::Error(e.to_string())),
        };
        on_event(WsEvent::Open);
        loop {
            tokio::select! {
                incoming = ws.next() => match incoming {
                    Some(Ok(Message::Text(t))) => on_event(WsEvent::Message(t)),
                    Some(Ok(Message::Binary(b))) => on_event(WsEvent::Message(String::from_utf8_lossy(&b).into_owned())),
                    Some(Ok(Message::Close(frame))) => {
                        let (code, reason) = frame.map(|f| (Some(u16::from(f.code)), f.reason.to_string())).unwrap_or((None, String::new()));
                        return on_event(WsEvent::Close(code, reason));
                    }
                    Some(Ok(_)) => {} // ping / pong are answered by the library
                    Some(Err(e)) => return on_event(WsEvent::Error(e.to_string())),
                    None => return on_event(WsEvent::Close(None, String::new())),
                },
                out = rx.recv() => match out {
                    Some(Out::Text(t)) => {
                        if let Err(e) = ws.send(Message::text(t)).await {
                            return on_event(WsEvent::Error(e.to_string()));
                        }
                    }
                    Some(Out::Close) => {
                        let _ = ws.close(None).await;
                        return;
                    }
                    None => return, // handle dropped
                },
            }
        }
    });
    WsHandle { tx, task, closing: std::sync::atomic::AtomicBool::new(false) }
}
