//! 转发白名单页:管理允许被拨号的目标列表(uunet 被控端唯一的准入项)。
//!
//! 每条精确匹配 IP:port:proto,空表 = 全部拒绝;编辑实时写
//! `~/.config/uunet/host.json`,转发拨号前实时读取,无需重启。

use crate::app::Page;
use crate::host::settings::{self, Target};
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Layout, Rect},
        style::{Color, Style, Stylize},
        text::Line,
        widgets::{Block, Paragraph, Widget},
    },
};
use std::cmp::min;

const WHITELIST_FIELDS: [&str; 4] = ["名称", "协议", "地址", "端口"];

#[derive(Default, Props)]
pub struct HostPageProps {
    /// 父级页面状态;Esc/q 写入 Page::Devices 返回设备列表。
    pub page: Option<State<Page>>,
}

#[component]
pub fn HostPage(props: &mut HostPageProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut page = props.page.clone().expect("HostPage 组件需要 page 属性");
    let mut selection = hooks.use_state(|| 0_usize);
    let mut form_open = hooks.use_state(|| false);
    let mut field_idx = hooks.use_state(|| 0_usize);
    let mut f_name = hooks.use_state(String::new);
    let mut f_proto = hooks.use_state(|| "tcp".to_string());
    let mut f_host = hooks.use_state(String::new);
    let mut f_port = hooks.use_state(String::new);
    let mut form_error = hooks.use_state(String::new);
    let mut confirm_idx = hooks.use_state(|| None::<usize>);
    let mut host_state = hooks.use_state(|| "关闭".to_string());
    let mut connections = hooks.use_state(|| (0_usize, 0_u64, 0_u64));

    // 被控状态与连接统计:后台循环刷新(与 devices 页同模式)。
    {
        let mut host_state = host_state.clone();
        hooks.use_future(async move {
            loop {
                let text = match crate::host::running_handle() {
                    Some(handle) => match &*handle.state.borrow() {
                        crate::host::HostState::Off => "关闭",
                        crate::host::HostState::Connecting => "连接中",
                        crate::host::HostState::Online => "在线",
                        crate::host::HostState::Reconnecting => "重连中",
                        crate::host::HostState::Offline => "离线",
                    }
                    .to_string(),
                    None => "未启动(--host)".into(),
                };
                host_state.set(text);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }
    {
        let mut connections = connections.clone();
        hooks.use_future(async move {
            loop {
                let stats = crate::host::access::current_status();
                connections.set((stats.connections, stats.sent, stats.received));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    let mut exit = hooks.use_exit();
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        if confirm_idx.get().is_some() {
            return EventResult::Ignored; // 删除确认弹窗期间交给内置弹窗
        }
        let whitelist_len = settings::shared().read().unwrap().whitelist.len();
        if form_open.get() {
            return handle_form_key(
                key.code,
                &mut field_idx,
                &mut form_open,
                &mut f_name,
                &mut f_proto,
                &mut f_host,
                &mut f_port,
                &mut form_error,
                &mut selection,
            );
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                page.set(Page::Devices);
                EventResult::Consumed
            }
            KeyCode::Up if whitelist_len > 0 => {
                selection.set(selection.get().saturating_sub(1) % whitelist_len);
                EventResult::Consumed
            }
            KeyCode::Down if whitelist_len > 0 => {
                selection.set((selection.get() + 1) % whitelist_len);
                EventResult::Consumed
            }
            KeyCode::Char(' ') if whitelist_len > 0 => {
                let shared = settings::shared();
                let mut settings = shared.write().unwrap();
                let index = min(selection.get(), settings.whitelist.len().saturating_sub(1));
                if let Some(target) = settings.whitelist.get_mut(index) {
                    target.proto =
                        if target.proto == "tcp" { "udp".to_string() } else { "tcp".to_string() };
                }
                if let Err(error) = settings::save(&settings) {
                    tracing::error!(%error, "保存白名单失败");
                }
                EventResult::Consumed
            }
            KeyCode::Char('a') => {
                field_idx.set(0);
                f_name.set(String::new());
                f_proto.set("tcp".into());
                f_host.set(String::new());
                f_port.set(String::new());
                form_error.set(String::new());
                form_open.set(true);
                EventResult::Consumed
            }
            KeyCode::Char('d') if whitelist_len > 0 => {
                confirm_idx.set(Some(min(selection.get(), whitelist_len - 1)));
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let settings_value = settings::shared().read().unwrap().clone();
    let selected_value = min(selection.get(), settings_value.whitelist.len().saturating_sub(1));
    let form_rows = build_form_rows(
        field_idx.get(),
        &f_name.read().clone(),
        &f_proto.read().clone(),
        &f_host.read().clone(),
        &f_port.read().clone(),
    );
    let form_error_text = form_error.read().clone();
    let show_form = form_open.get();
    let confirm_target = confirm_idx
        .get()
        .and_then(|index| settings_value.whitelist.get(index))
        .cloned();
    let stats_value = connections.get();
    let host_state_text = host_state.read().clone();

    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Percentage(100),
        height: Constraint::Percentage(100),
    ) {
        widget(HostHeaderWidget { state: host_state_text, connections: stats_value })
        widget(WhitelistWidget {
            settings: settings_value,
            selected: selected_value,
            form_rows,
            error: form_error_text,
            show_form,
        })
        if let Some(target) = confirm_target {
            ConfirmModal(
                open: true,
                title: Line::from(" 删除白名单条目 ").red().bold(),
                content: TextParagraph::from(format!(
                    "确定删除「{} {}:{}」吗?",
                    target.proto, target.host, target.port
                )),
                confirm_text: "删除".to_string(),
                cancel_text: "取消".to_string(),
                on_confirm: move |_| {
                    let Some(index) = confirm_idx.get() else {
                        return;
                    };
                    let shared = settings::shared();
                    {
                        let mut settings = shared.write().unwrap();
                        if index < settings.whitelist.len() {
                            settings.whitelist.remove(index);
                        }
                        if let Err(error) = settings::save(&settings) {
                            tracing::error!(%error, "保存白名单失败");
                        }
                    }
                },
                on_cancel: move |_| {
                    confirm_idx.set(None);
                },
            )
        }
        widget(HostHelpWidget)
    })
}

#[allow(clippy::too_many_arguments)]
fn handle_form_key(
    code: KeyCode,
    field_idx: &mut State<usize>,
    form_open: &mut State<bool>,
    f_name: &mut State<String>,
    f_proto: &mut State<String>,
    f_host: &mut State<String>,
    f_port: &mut State<String>,
    form_error: &mut State<String>,
    selection: &mut State<usize>,
) -> EventResult {
    const FIELDS: usize = WHITELIST_FIELDS.len();
    match code {
        KeyCode::Esc => {
            form_open.set(false);
            EventResult::Consumed
        }
        KeyCode::Up => {
            let idx = field_idx.get();
            field_idx.set(if idx == 0 { FIELDS - 1 } else { idx - 1 });
            EventResult::Consumed
        }
        KeyCode::Down | KeyCode::Tab => {
            field_idx.set((field_idx.get() + 1) % FIELDS);
            EventResult::Consumed
        }
        KeyCode::Enter => {
            if field_idx.get() + 1 < FIELDS {
                field_idx.set(field_idx.get() + 1);
                EventResult::Consumed
            } else {
                let form = Target {
                    name: f_name.read().clone(),
                    proto: f_proto.read().clone(),
                    host: f_host.read().clone(),
                    port: f_port.read().parse().unwrap_or(0),
                };
                if form.name.trim().is_empty() {
                    form_error.set("名称不能为空".into());
                    return EventResult::Consumed;
                }
                if form.host.trim().parse::<std::net::IpAddr>().is_err() {
                    form_error.set("地址须为合法 IP(暂不支持域名)".into());
                    return EventResult::Consumed;
                }
                if form.port == 0 {
                    form_error.set("端口应为 1–65535".into());
                    return EventResult::Consumed;
                }
                let shared = settings::shared();
                let mut settings = shared.write().unwrap();
                settings.whitelist.push(form);
                if let Err(error) = settings::save(&settings) {
                    form_error.set(format!("保存失败:{error}"));
                    return EventResult::Consumed;
                }
                drop(settings);
                selection.set(0);
                form_open.set(false);
                EventResult::Consumed
            }
        }
        KeyCode::Char('t') if field_idx.get() == 1 => {
            f_proto.set(if f_proto.read().as_str() == "tcp" {
                "udp".to_string()
            } else {
                "tcp".to_string()
            });
            EventResult::Consumed
        }
        KeyCode::Backspace => {
            let idx = field_idx.get();
            let field = match idx {
                0 => f_name,
                2 => f_host,
                _ => f_port,
            };
            let mut value = field.read().clone();
            value.pop();
            field.set(value);
            EventResult::Consumed
        }
        KeyCode::Char(c) => {
            let idx = field_idx.get();
            if idx == 1 {
                f_proto.set(if f_proto.read().as_str() == "tcp" {
                    "udp".to_string()
                } else {
                    "tcp".to_string()
                });
                return EventResult::Consumed;
            }
            let field = match idx {
                0 => f_name,
                2 => f_host,
                _ => f_port,
            };
            let mut value = field.read().clone();
            if value.chars().count() < 40 {
                value.push(c);
                field.set(value);
            }
            EventResult::Consumed
        }
        _ => EventResult::Ignored,
    }
}

fn build_form_rows(
    active: usize,
    name: &str,
    proto: &str,
    host: &str,
    port: &str,
) -> Vec<(String, String, bool)> {
    let values = [
        name.to_string(),
        proto.to_ascii_uppercase(),
        host.to_string(),
        port.to_string(),
    ];
    WHITELIST_FIELDS
        .iter()
        .enumerate()
        .map(|(idx, label)| (label.to_string(), values[idx].clone(), idx == active))
        .collect()
}

/// 标题 + 被控状态 + 连接统计。
#[derive(Clone)]
struct HostHeaderWidget {
    state: String,
    connections: (usize, u64, u64),
}

impl Widget for HostHeaderWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        Paragraph::new(Line::from(format!(
            " uunet 转发白名单   被控:{}   连接 {} 条 ↑{}/↓{}",
            self.state, self.connections.0, self.connections.1, self.connections.2
        ))
        .cyan()
        .bold())
        .render(area, buf);
    }
}

/// 白名单表(+ 可选的追加表单)。
#[derive(Clone)]
struct WhitelistWidget {
    settings: settings::Settings,
    selected: usize,
    form_rows: Vec<(String, String, bool)>,
    error: String,
    show_form: bool,
}

impl Widget for WhitelistWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let block = Block::bordered()
            .title(Line::from(" 拨号白名单(精确匹配,空表 = 全拒) ").cyan())
            .border_style(Style::new().cyan());
        let inner = block.inner(area);
        block.render(area, buf);

        let columns = [
            Constraint::Percentage(32),
            Constraint::Length(8),
            Constraint::Percentage(32),
            Constraint::Length(8),
        ];
        let mut y = inner.y;
        let header: [Rect; 4] = Layout::horizontal(columns).areas(Rect::new(inner.x, y, inner.width, 1));
        let labels = ["名称", "协议", "地址", "端口"];
        for (idx, chunk) in header.iter().enumerate() {
            Paragraph::new(Line::from(labels[idx]).dark_gray()).render(*chunk, buf);
        }
        y += 1;
        for (index, target) in self.settings.whitelist.iter().enumerate() {
            let selected = index == self.selected;
            let row_rect = Rect::new(inner.x, y, inner.width, 1);
            let cells: [Rect; 4] = Layout::horizontal(columns).areas(row_rect);
            let values = [
                target.name.clone(),
                target.proto.to_ascii_uppercase(),
                target.host.clone(),
                target.port.to_string(),
            ];
            for (idx, value) in values.iter().enumerate() {
                let mut style = Style::new();
                if selected {
                    style = style.bg(Color::DarkGray).bold();
                }
                Paragraph::new(Line::from(format!(" {value}")).style(style))
                    .render(cells[idx], buf);
            }
            y += 1;
        }
        if self.settings.whitelist.is_empty() {
            Paragraph::new(Line::from("白名单为空:任何转发拨号都会被拒绝,按 a 添加".dark_gray()).centered())
                .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
                .render(Rect::new(inner.x, y, inner.width, 1), buf);
            y += 1;
        }
        if self.show_form {
            y += 1;
            let form_rect = Rect::new(inner.x, y, inner.width, WHITELIST_FIELDS.len() as u16 + 1);
            let rows: [Rect; 4] = Layout::vertical([Constraint::Length(1); 4]).areas(form_rect);
            for ((label, value, active), row) in self.form_rows.iter().zip(rows.iter()) {
                let marker = if *active { "▸ " } else { "  " };
                let style = if *active {
                    Style::new().cyan().bold()
                } else {
                    Style::new()
                };
                let cursor = if *active { "_" } else { "" };
                Paragraph::new(
                    Line::from(format!("{marker}{label:<6} {value}{cursor}")).style(style),
                )
                .render(*row, buf);
            }
        }
        if !self.error.is_empty() {
            Paragraph::new(Line::from(self.error).red()).render(
                Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                buf,
            );
        }
    }
}

/// 底部键位说明。
#[derive(Clone)]
struct HostHelpWidget;

impl Widget for HostHelpWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        Paragraph::new(
            Line::from(
                " ↑/↓ 选择   空格 切协议   a 添加   d 删除   Esc/q 返回设备 ".dark_gray(),
            )
            .centered(),
        )
        .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
        .render(area, buf);
    }
}
