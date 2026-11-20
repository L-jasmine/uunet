//! Devices page: the machines in the account's network (tailnet 意义上的
//! machines). Select one and Enter opens its port-map page.

use crate::app::Page;
use crate::backend::{self, Device};
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Layout, Rect},
        style::{Style, Stylize},
        text::Line,
        widgets::{Block, Paragraph, Widget},
    },
};
use std::cmp::min;

#[derive(Default, Props)]
pub struct DevicesProps {
    /// Parent's page state; Enter writes `Page::PortMap(id)` into it.
    /// `Option` mirrors the framework's own props (handles have no Default).
    pub page: Option<State<Page>>,
}

/// 被控状态的展示文本(未启动 --host 时为空)。
fn host_status_text() -> String {
    match crate::host::running_handle() {
        Some(handle) => match &*handle.state.borrow() {
            crate::host::HostState::Off => "",
            crate::host::HostState::Connecting => "被控:连接中",
            crate::host::HostState::Online => "被控:在线",
            crate::host::HostState::Reconnecting => "被控:重连中",
            crate::host::HostState::Offline => "被控:离线",
        }
        .to_string(),
        None => String::new(),
    }
}

#[component]
pub fn Devices(props: &mut DevicesProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut page = props.page.clone().expect("Devices 组件需要 page 属性");
    let devices = hooks.use_state(Vec::new);
    let mut selected = hooks.use_state(|| 0_usize);
    let error = hooks.use_state(String::new);
    let host_status = hooks.use_state(String::new);

    // 被控状态行:500ms 轮询 watch(与统计刷新同节奏)。
    {
        let mut host_status = host_status.clone();
        hooks.use_future(async move {
            loop {
                host_status.set(host_status_text());
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    let mut load_devices = devices.clone();
    let mut load_error = error.clone();
    hooks.use_future(async move {
        match backend::list_devices().await {
            Ok(list) => {
                load_error.set(String::new());
                load_devices.set(list);
            }
            Err(e) => load_error.set(format!("获取设备列表失败:{e:#}")),
        }
    });

    let mut exit = hooks.use_exit();
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        let count = devices.read().len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                exit();
                EventResult::Consumed
            }
            KeyCode::Up => {
                if count > 0 {
                    selected.set(selected.get().saturating_sub(1) % count);
                }
                EventResult::Consumed
            }
            KeyCode::Down => {
                if count > 0 {
                    selected.set((selected.get() + 1) % count);
                }
                EventResult::Consumed
            }
            KeyCode::Enter => {
                let current = devices.read().clone();
                if let Some(device) = current.get(selected.get()) {
                    page.set(Page::PortMap(device.id.clone()));
                }
                EventResult::Consumed
            }
            KeyCode::Char('h') => {
                page.set(Page::Host);
                EventResult::Consumed
            }
            KeyCode::Char('r') => {
                // 键位事件不能 await;丢到后台任务,完成后回写状态。
                let mut fresh = devices.clone();
                let mut err = error.clone();
                let mut sel = selected.clone();
                let current = selected.get();
                tokio::spawn(async move {
                    match backend::list_devices().await {
                        Ok(list) => {
                            err.set(String::new());
                            sel.set(min(current, list.len().saturating_sub(1)));
                            fresh.set(list);
                        }
                        Err(e) => err.set(format!("刷新失败:{e:#}")),
                    }
                });
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let devices_value = devices.read().clone();
    let error_value = error.read().clone();
    let selected_value = selected.get().min(devices_value.len().saturating_sub(1));

    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Percentage(100),
        height: Constraint::Percentage(100),
    ) {
        // widget 适配器自身的默认约束是 Percentage(100),多 child 会被
        // Layout 平分(小终端上下大片留白);每段外包 View 指定高度。
        element!(View(height: Constraint::Length(1)) {
            widget(DevicesHeaderWidget {
                total: devices_value.len(),
                online: devices_value.iter().filter(|d| d.online).count(),
                host_status: host_status.read().clone(),
            })
        })
        element!(View(height: Constraint::Min(0)) {
            widget(DevicesWidget { devices: devices_value, selected: selected_value, error: error_value })
        })
        element!(View(height: Constraint::Length(1)) {
            widget(DevicesHelpWidget)
        })
    })
}

/// One-line header: page title with device counts.
#[derive(Clone)]
struct DevicesHeaderWidget {
    total: usize,
    online: usize,
    host_status: String,
}

impl Widget for DevicesHeaderWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let suffix = if self.host_status.is_empty() {
            String::new()
        } else {
            format!("   {}", self.host_status)
        };
        Paragraph::new(
            Line::from(format!(
                " uunet 设备列表   {}/{}{}",
                self.online, self.total, suffix
            ))
            .cyan()
            .bold(),
        )
        .render(area, buf);
    }
}

/// Bordered device list; when rows exceed the visible area the viewport
/// follows the selection (kept near center).
#[derive(Clone)]
struct DevicesWidget {
    devices: Vec<Device>,
    selected: usize,
    /// 非空时显示在列表区(获取失败等原因)。
    error: String,
}

impl Widget for DevicesWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let block = Block::bordered()
            .title(Line::from(" 我的设备 ").cyan())
            .border_style(Style::new().cyan());
        let inner = block.inner(area);
        block.render(area, buf);

        let columns = [
            Constraint::Length(3),
            Constraint::Percentage(30),
            Constraint::Length(10),
            Constraint::Percentage(30),
            Constraint::Length(8),
        ];
        // 可视数据行数(减列表头);此前误用含表头的高度,末行会画到边框外。
        let visible = inner.height.saturating_sub(1) as usize;
        // 视口跟随选中项并尽量居中;行数不足一屏时不动。
        let skip = if self.devices.len() <= visible {
            0
        } else {
            self.selected
                .saturating_sub(visible / 2)
                .min(self.devices.len() - visible)
        };

        let header: [Rect; 5] = Layout::horizontal(columns).areas(inner);
        let labels = ["", "名称", "系统", "信息", "状态"];
        for (idx, chunk) in header.iter().enumerate() {
            Paragraph::new(Line::from(labels[idx]).dark_gray()).render(*chunk, buf);
        }
        let rows_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );
        for (row_idx, device) in self.devices.iter().skip(skip).take(visible).enumerate() {
            // One terminal row per device; column rects must be cut from that
            // single row, not from the whole rows area (rows would overwrite
            // each other at the top).
            let row_rect = Rect::new(
                rows_area.x,
                rows_area.y + row_idx as u16,
                rows_area.width,
                1,
            );
            let cells: [Rect; 5] = Layout::horizontal(columns).areas(row_rect);
            let is_selected = skip + row_idx == self.selected;
            let values = [
                if device.online {
                    "●".to_string()
                } else {
                    "○".to_string()
                },
                device.name.clone(),
                device.os.clone(),
                device.detail.clone(),
                if device.online { "在线" } else { "离线" }.to_string(),
            ];
            for (col_idx, value) in values.iter().enumerate() {
                let mut style = if device.online {
                    Style::new().green()
                } else {
                    Style::new().dark_gray()
                };
                if is_selected {
                    style = style.reversed();
                }
                Paragraph::new(Line::from(format!(" {value}")))
                    .style(style)
                    .render(cells[col_idx], buf);
            }
        }
        if self.devices.is_empty() {
            let message = if self.error.is_empty() {
                "正在获取设备列表…".to_string()
            } else {
                self.error
            };
            Paragraph::new(Line::from(message.dark_gray()).centered())
                .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
                .render(rows_area, buf);
        }
    }
}

/// One-line key help pinned to the bottom.
#[derive(Clone)]
struct DevicesHelpWidget;

impl Widget for DevicesHelpWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        Paragraph::new(
            Line::from(" ↑/↓ 选择   Enter 端口映射   h 被控   r 刷新   q 退出 ".dark_gray())
                .centered(),
        )
        .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
        .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_kit::ratatui::buffer::Buffer;

    #[test]
    fn devices_widget_renders_rows_and_status() {
        let devices = vec![
            Device {
                id: "dev-1".into(),
                name: "office-nas".into(),
                os: "Linux".into(),
                detail: "2.1.0".into(),
                online: true,
            },
            Device {
                id: "dev-2".into(),
                name: "lab-server".into(),
                os: "Linux".into(),
                detail: "2.1.0".into(),
                online: false,
            },
        ];
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        DevicesWidget {
            devices,
            selected: 1,
            error: String::new(),
        }
        .render(area, &mut buf);

        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        // Wide (CJK) characters occupy two buffer cells; collapse spaces so
        // labels can be matched regardless of column packing.
        let joined = rows.join("\n").replace(' ', "");
        for label in ["名称", "系统", "office-nas", "lab-server", "2.1.0", "离线"] {
            assert!(joined.contains(label), "missing {label} in:\n{joined}");
        }
        assert!(rows[0].starts_with('┌') && rows[0].ends_with('┐'));
    }

    fn device(index: usize) -> Device {
        Device {
            id: format!("dev-{index}"),
            name: format!("node-{index}"),
            os: "Linux".into(),
            detail: "2.1.0".into(),
            online: true,
        }
    }

    #[test]
    fn devices_widget_scrolls_to_keep_selection_visible() {
        // 6 行高 = 边框 2 + 表头 1 + 数据 3;5 台设备必然溢出。
        let devices: Vec<_> = (0..5).map(device).collect();
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        DevicesWidget {
            devices,
            selected: 4,
            error: String::new(),
        }
        .render(area, &mut buf);

        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        let joined = rows.join("\n").replace(' ', "");
        // 视口跟随选中项:末台可见,首台滚出。
        assert!(
            joined.contains("node-4"),
            "selected row missing in:\n{joined}"
        );
        assert!(
            !joined.contains("node-0"),
            "stale row still rendered in:\n{joined}"
        );
        // 数据不越过底边框:边框行之外没有 node-* 残留。
        assert!(rows[area.height as usize - 1].contains('└'));
    }
}
