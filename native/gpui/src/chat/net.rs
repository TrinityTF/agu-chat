//! The UI's own network handle: a small tokio runtime plus one reqwest client with Agu Chat's honest user agent.
//! Emote sets, 7TV cosmetics and image bytes are fetched here (the JS did these with the browser's fetch);
//! everything platform-related still goes through the backend bridge. Futures are executor-agnostic JoinHandles,
//! so GPUI tasks can `.await` them.
use std::future::Future;
use std::sync::Arc;

pub struct Net {
    pub rt: tokio::runtime::Runtime,
    pub client: reqwest::Client,
}

impl Net {
    pub fn new(version: &str) -> Arc<Net> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("agu-ui-net").build().expect("tokio runtime");
        let client = reqwest::Client::builder()
            .user_agent(agu_chat_core::backend::user_agent(version))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("http client");
        Arc::new(Net { rt, client })
    }

    /// Run `fut` on the net runtime; the returned future resolves with its output (None if it panicked).
    pub fn run<T: Send + 'static>(&self, fut: impl Future<Output = T> + Send + 'static) -> impl Future<Output = Option<T>> {
        let h = self.rt.spawn(fut);
        async move { h.await.ok() }
    }
}
