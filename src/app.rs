//! Top-level component: login → devices → per-device port map.

use crate::devices::Devices;
use crate::login::Login;
use crate::portmap::PortMap;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::{Constraint, Direction};

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Login,
    Devices,
    PortMap(String),
}

#[component]
fn App(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let page = hooks.use_state(|| Page::Login);

    let current = page.read().clone();
    if current == Page::Login {
        element!(View(
            flex_direction: Direction::Vertical,
            width: Constraint::Percentage(100),
            height: Constraint::Percentage(100),
        ) {
            Login(page: page.clone())
        })
    } else if current == Page::Devices {
        element!(View(
            flex_direction: Direction::Vertical,
            width: Constraint::Percentage(100),
            height: Constraint::Percentage(100),
        ) {
            Devices(page: page.clone())
        })
    } else {
        let Page::PortMap(device_id) = current else {
            unreachable!("前两个分支已覆盖其余取值");
        };
        element!(View(
            flex_direction: Direction::Vertical,
            width: Constraint::Percentage(100),
            height: Constraint::Percentage(100),
        ) {
            PortMap(page: page.clone(), device_id: device_id)
        })
    }
}

/// Entry: run the fullscreen TUI until the user quits. Mouse capture is
/// required: built-in modals register pointer handlers and panic without it.
pub async fn run() -> anyhow::Result<()> {
    ratatui_kit::element!(App)
        .fullscreen_with(RunOptions {
            mouse: true,
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}
