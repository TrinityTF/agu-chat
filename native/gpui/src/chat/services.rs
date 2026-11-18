//! The shared, UI-thread services of the chat view: network, image cache, 7TV cosmetics, alerts. One instance
//! lives in a GPUI global; panes reach it with `cx.global::<Services>()`.
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agu_chat_core::protocol::AguBridge;
use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui::{App, Global};

use super::alerts::Alerts;
use super::images::{Images, ImagesHandle};
use super::net::Net;
use super::previews::Previews;
use super::twitch::cosmetics::Cosmetics;
use super::twitch::emotes::init_cache;
use super::twitch::seven_events::SevenEvents;

pub struct ServicesInner {
    pub net: Arc<Net>,
    pub images: ImagesHandle,
    pub cosmetics: Cosmetics,
    /// live 7TV emote set changes (EventAPI), shared by every pane
    pub seven_events: SevenEvents,
    pub alerts: Alerts,
    /// link preview cards, shared by every pane
    pub previews: Previews,
    pub bridge: Arc<dyn AguBridge>,
    pub data_dir: PathBuf,
    pub window_active: Cell<bool>,
    pub hwnd: Cell<isize>,
}

#[derive(Clone)]
pub struct Services(pub Rc<ServicesInner>);
impl Global for Services {}

impl std::ops::Deref for Services {
    type Target = ServicesInner;
    fn deref(&self) -> &ServicesInner {
        &self.0
    }
}

/// Set up the services and the task that turns finished image downloads into GPU images.
pub fn init(cx: &mut App, bridge: Arc<dyn AguBridge>, data_dir: PathBuf) -> Services {
    init_cache(data_dir.clone());
    let version = bridge.app_info().version;
    let net = Net::new(&version);
    let (tx, mut rx) = unbounded::<()>();
    let images = ImagesHandle(Images::new(&net, Arc::new(move || {
        let _ = tx.unbounded_send(());
    }), &data_dir));
    let seven_events = SevenEvents::new(net.clone(), cx);
    let svc = Services(Rc::new(ServicesInner {
        net,
        images: images.clone(),
        cosmetics: Cosmetics::new(),
        seven_events,
        alerts: Alerts::new(),
        previews: Previews::new(),
        bridge,
        data_dir,
        window_active: Cell::new(true),
        hwnd: Cell::new(0),
    }));
    cx.set_global(svc.clone());
    cx.set_global(images.clone());
    cx.spawn(async move |cx| {
        while rx.next().await.is_some() {
            while let Ok(Some(())) = rx.try_next() {}
            loop {
                let more = cx.update(|cx| {
                    let more = images.0.pump();
                    cx.refresh_windows();
                    more
                });
                match more {
                    Ok(true) => cx.background_executor().timer(Duration::from_millis(2)).await,
                    _ => break,
                }
            }
        }
    })
    .detach();
    svc
}
