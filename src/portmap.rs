//! 端口映射页:真实规则(按设备持久化)+ 映射会话控制。
//!
//! `c` 建立到当前设备的映射会话(会话内自动监听 enabled 规则),
//! 再按 `c` 断开。规则增删改即时持久化,会话运行中 500ms 内生效。

use crate::app::Page;
use crate::backend;
use crate::controller;
use crate::forward::{Protocol, Rule};
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

const FORM_FIELDS: [&str; 5] = ["名称", "协议", "本地端口", "目标地址", "目标端口"];
const FORM_HEIGHT: u16 = 9;

#[derive(Default, Props)]
pub struct PortMapProps {
    /// 父级页面状态;Esc 写入 Page::Devices 返回设备列表。
    pub page: Option<State<Page>>,
    /// 本页管理的目标设备。
    pub device_id: Option<String>,
}

#[component]
pub fn PortMap(props: &mut PortMapProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut page = props.page.clone().expect("PortMap 组件需要 page 属性");
    let device_id = props
        .device_id
        .clone()
        .expect("PortMap 组件需要 device_id 属性");
    let device = backend::device_by_id(&device_id);
    let mut rules = hooks.use_state(Vec::new);
    let mut selected = hooks.use_state(|| 0_usize);
    let mut form_open = hooks.use_state(|| false);
    let mut field_idx = hooks.use_state(|| 0_usize);
    let mut f_name = hooks.use_state(String::new);
    let mut f_proto = hooks.use_state(|| Protocol::Tcp);
    let mut f_local = hooks.use_state(String::new);
    let mut f_target = hooks.use_state(String::new);
    let mut f_port = hooks.use_state(String::new);
    let mut form_error = hooks.use_state(String::new);
    let mut confirm_id = hooks.use_state(|| None::<u64>);
    let mut session_status = hooks.use_state(|| String::new());

    let device_id_for_load = device_id.clone();
    let mut load_rules = rules.clone();
    hooks.use_future(async move {
        load_rules.set(controller::load_rules(&device_id_for_load));
    });

    // 会话状态轮询(controller 服务写入,500ms 同节奏)。
    {
        let mut session_status = session_status.clone();
        let device_id_for_status = device_id.clone();
        hooks.use_future(async move {
            loop {
                let status = controller::mapping_status(&device_id_for_status);
                let running = controller::is_running(&device_id_for_status);
                let text = if running {
                    match status.error {
                        Some(error) => format!("{}({error})", status.state),
                        None => status.state.clone(),
                    }
                } else if status.state == "失败" {
                    format!("上次会话失败:{}", status.error.clone().unwrap_or_default())
                } else {
                    "未连接".to_string()
                };
                session_status.set(text);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    let mut exit = hooks.use_exit();
    let mut rules_for_modal = rules.clone();
    let mut selected_for_modal = selected.clone();
    let device_id_for_modal = device_id.clone();
    let device_id_for_form = device_id.clone();
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        if confirm_id.get().is_some() {
            return EventResult::Ignored;
        }
        if form_open.get() {
            return handle_form_key(
                key.code,
                &device_id_for_form,
                &mut field_idx,
                &mut form_open,
                &mut f_name,
                &mut f_proto,
                &mut f_local,
                &mut f_target,
                &mut f_port,
                &mut form_error,
                &mut rules,
                &mut selected,
            );
        }
        let count = rules.read().len();
        match key.code {
            KeyCode::Char('q') => {
                exit();
                EventResult::Consumed
            }
            KeyCode::Esc => {
                page.set(Page::Devices);
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
            KeyCode::Char(' ') => {
                let current = rules.read().clone();
                if let Some(rule) = current.get(selected.get()) {
                    let mut toggled = rule.clone();
                    toggled.enabled = !toggled.enabled;
                    match persist_rules(&device_id_for_form, |mut all| {
                        replace_rule(all, toggled.clone())
                    }) {
                        Ok(all) => rules.set(all),
                        Err(error) => tracing::error!(%error, "保存规则失败"),
                    }
                }
                EventResult::Consumed
            }
            KeyCode::Char('d') => {
                let current = rules.read().clone();
                if let Some(rule) = current.get(selected.get()) {
                    confirm_id.set(Some(rule.id));
                }
                EventResult::Consumed
            }
            KeyCode::Char('c') => {
                if controller::is_running(&device_id_for_form) {
                    controller::disconnect(&device_id_for_form);
                } else if let Err(error) = controller::connect(&device_id_for_form) {
                    tracing::error!(%error, "启动映射会话失败");
                }
                EventResult::Consumed
            }
            KeyCode::Char('a') => {
                field_idx.set(0);
                f_name.set(String::new());
                f_proto.set(Protocol::Tcp);
                f_local.set(String::new());
                f_target.set(String::new());
                f_port.set(String::new());
                form_error.set(String::new());
                form_open.set(true);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let header_title = match &device {
        Some(d) => format!(" uunet 端口映射 · {}", d.name),
        None => " uunet 端口映射".to_string(),
    };
    let rules_value = rules.read().clone();
    let selected_value = selected.get().min(rules_value.len().saturating_sub(1));
    let name_value = f_name.read().clone();
    let proto_value = f_proto.get();
    let local_value = f_local.read().clone();
    let target_value = f_target.read().clone();
    let port_value = f_port.read().clone();
    let form_rows = build_form_rows(
        field_idx.get(),
        &name_value,
        proto_value,
        &local_value,
        &target_value,
        &port_value,
    );
    let form_error_text = form_error.read().clone();
    let show_form = form_open.get();
    let session_text = session_status.read().clone();
    let running = controller::is_running(&device_id);
    let confirm_rule = confirm_id
        .get()
        .and_then(|id| rules_value.iter().find(|r| r.id == id))
        .cloned();

    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Percentage(100),
        height: Constraint::Percentage(100),
    ) {
        widget(HeaderWidget {
            title: header_title,
            session: session_text,
            running,
            total: rules_value.len(),
            enabled: rules_value.iter().filter(|r| r.enabled).count(),
        })
        widget(RulesWidget {
            rules: rules_value.clone(),
            selected: selected_value,
            reserved_bottom: if show_form { FORM_HEIGHT } else { 0 },
        })
        if show_form {
            widget(FormWidget { fields: form_rows, error: form_error_text })
        }
        if let Some(rule) = confirm_rule {
            ConfirmModal(
                open: true,
                title: Line::from(" 删除规则 ").red().bold(),
                content: TextParagraph::from(format!("确定删除「{}」吗?", rule.name)),
                confirm_text: "删除".to_string(),
                cancel_text: "取消".to_string(),
                on_confirm: move |_| {
                    let id = rule.id;
                    if let Err(error) =
                        persist_rules(&device_id_for_modal, move |mut all| {
                            all.retain(|r| r.id != id);
                            all
                        })
                    {
                        tracing::error!(%error, "保存规则失败");
                    }
                    confirm_id.set(None);
                    let fresh = controller::load_rules(&device_id_for_modal);
                    selected_for_modal
                        .set(min(selected_for_modal.get(), fresh.len().saturating_sub(1)));
                    rules_for_modal.set(fresh);
                },
                on_cancel: move |_| {
                    confirm_id.set(None);
                },
            )
        }
        widget(HelpWidget)
    })
}

/// 以闭包原子更新规则文件;错误转成可直接展示的字符串。
fn persist_rules<F>(device_id: &str, edit: F) -> Result<Vec<Rule>, String>
where
    F: FnOnce(Vec<Rule>) -> Vec<Rule>,
{
    let mut all = controller::load_rules(device_id);
    all = edit(all);
    controller::save_rules(device_id, &all).map_err(|error| format!("{error:#}"))?;
    Ok(all)
}

fn replace_rule(all: Vec<Rule>, replacement: Rule) -> Vec<Rule> {
    all.into_iter()
        .map(|rule| {
            if rule.id == replacement.id {
                replacement.clone()
            } else {
                rule
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn handle_form_key(
    code: KeyCode,
    device_id: &str,
    field_idx: &mut State<usize>,
    form_open: &mut State<bool>,
    f_name: &mut State<String>,
    f_proto: &mut State<Protocol>,
    f_local: &mut State<String>,
    f_target: &mut State<String>,
    f_port: &mut State<String>,
    form_error: &mut State<String>,
    rules: &mut State<Vec<Rule>>,
    selected: &mut State<usize>,
) -> EventResult {
    const FIELDS: usize = FORM_FIELDS.len();
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
                submit_form(
                    device_id,
                    f_name,
                    f_proto,
                    f_local,
                    f_target,
                    f_port,
                    form_error,
                    form_open,
                    rules,
                    selected,
                )
            }
        }
        KeyCode::Char('t') if field_idx.get() == 1 => {
            f_proto.set(match f_proto.get() {
                Protocol::Tcp => Protocol::Udp,
                Protocol::Udp => Protocol::Tcp,
            });
            EventResult::Consumed
        }
        KeyCode::Backspace => {
            let idx = field_idx.get();
            let mut value = input_field(idx, f_name, f_local, f_target, f_port)
                .read()
                .clone();
            value.pop();
            set_input_field(idx, value, f_name, f_local, f_target, f_port);
            EventResult::Consumed
        }
        KeyCode::Char(c) => {
            let idx = field_idx.get();
            if idx == 1 {
                f_proto.set(match f_proto.get() {
                    Protocol::Tcp => Protocol::Udp,
                    Protocol::Udp => Protocol::Tcp,
                });
                return EventResult::Consumed;
            }
            let mut value = input_field(idx, f_name, f_local, f_target, f_port)
                .read()
                .clone();
            if value.chars().count() < 40 {
                value.push(c);
                set_input_field(idx, value, f_name, f_local, f_target, f_port);
            }
            EventResult::Consumed
        }
        _ => EventResult::Ignored,
    }
}

fn input_field<'a>(
    idx: usize,
    f_name: &'a State<String>,
    f_local: &'a State<String>,
    f_target: &'a State<String>,
    f_port: &'a State<String>,
) -> &'a State<String> {
    match idx {
        0 => f_name,
        2 => f_local,
        3 => f_target,
        _ => f_port,
    }
}

fn set_input_field(
    idx: usize,
    value: String,
    f_name: &mut State<String>,
    f_local: &mut State<String>,
    f_target: &mut State<String>,
    f_port: &mut State<String>,
) {
    match idx {
        0 => f_name.set(value),
        2 => f_local.set(value),
        3 => f_target.set(value),
        _ => f_port.set(value),
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_form(
    device_id: &str,
    f_name: &State<String>,
    f_proto: &State<Protocol>,
    f_local: &State<String>,
    f_target: &State<String>,
    f_port: &State<String>,
    form_error: &mut State<String>,
    form_open: &mut State<bool>,
    rules: &mut State<Vec<Rule>>,
    selected: &mut State<usize>,
) -> EventResult {
    let mut rule = Rule {
        id: 0, // 由保存路径分配
        name: f_name.read().clone(),
        protocol: f_proto.get(),
        local_addr: controller::DEFAULT_LOCAL_ADDR,
        local_port: f_local.read().parse().unwrap_or(0),
        target: f_target.read().trim().parse().unwrap_or_else(|_| {
            // 非法地址先占位,validate 会拒绝并给出消息。
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        }),
        remote_port: f_port.read().parse().unwrap_or(0),
        enabled: true,
    };
    let mut all = controller::load_rules(device_id);
    rule.id = controller::next_rule_id(&all);
    match controller::validate_rule(&rule) {
        Ok(()) => {
            all.push(rule.clone());
            if let Err(error) = controller::save_rules(device_id, &all) {
                form_error.set(format!("保存失败:{error}"));
                return EventResult::Consumed;
            }
            form_open.set(false);
            selected.set(all.len().saturating_sub(1));
            rules.set(all);
        }
        Err(message) => form_error.set(message),
    }
    EventResult::Consumed
}

fn build_form_rows(
    active: usize,
    name: &str,
    protocol: Protocol,
    local: &str,
    target: &str,
    port: &str,
) -> Vec<(String, String, bool)> {
    let values = [
        name.to_string(),
        match protocol {
            Protocol::Tcp => "TCP".to_string(),
            Protocol::Udp => "UDP".to_string(),
        },
        local.to_string(),
        target.to_string(),
        port.to_string(),
    ];
    FORM_FIELDS
        .iter()
        .enumerate()
        .map(|(idx, label)| (label.to_string(), values[idx].clone(), idx == active))
        .collect()
}

/// 标题 + 会话状态 + 规则统计。
#[derive(Clone)]
struct HeaderWidget {
    title: String,
    session: String,
    running: bool,
    total: usize,
    enabled: usize,
}

impl Widget for HeaderWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let session_style = if self.running {
            Style::new().green()
        } else {
            Style::new().dark_gray()
        };
        Paragraph::new(Line::from(vec![
            format!(
                " {}   规则 {} 条,启用 {} 条   ",
                self.title, self.total, self.enabled
            )
            .into(),
            ratatui_kit::ratatui::text::Span::styled(
                format!("[{}]", self.session),
                session_style,
            ),
        ]))
        .render(area, buf);
    }
}

/// 带边框的规则表;超出可视区的行按页滚动。
#[derive(Clone)]
struct RulesWidget {
    rules: Vec<Rule>,
    selected: usize,
    /// 底部为追加表单保留的行数(0 = 无)。
    reserved_bottom: u16,
}

impl Widget for RulesWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let frame = if self.reserved_bottom > 0 {
            let chunks: [Rect; 2] = Layout::vertical([
                Constraint::Min(3),
                Constraint::Length(self.reserved_bottom),
            ])
            .areas(area);
            chunks[0]
        } else {
            area
        };
        let block = Block::bordered()
            .title(Line::from(" 端口映射规则 ").cyan())
            .border_style(Style::new().cyan());
        let inner = block.inner(frame);
        block.render(frame, buf);

        let columns = [
            Constraint::Length(3),
            Constraint::Percentage(22),
            Constraint::Length(6),
            Constraint::Length(11),
            Constraint::Percentage(30),
            Constraint::Length(8),
        ];
        let visible = inner.height.saturating_sub(1) as usize; // 减列表头
        let skip = if visible == 0 {
            0
        } else {
            self.selected / visible * visible
        };

        let header: [Rect; 6] = Layout::horizontal(columns).areas(inner);
        let labels = ["", "名称", "协议", "本地监听", "目标", "状态"];
        for (idx, chunk) in header.iter().enumerate() {
            Paragraph::new(Line::from(labels[idx]).dark_gray()).render(*chunk, buf);
        }
        let rows_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height.saturating_sub(1));
        for (row_idx, rule) in self.rules.iter().skip(skip).take(visible).enumerate() {
            let is_selected = skip + row_idx == self.selected;
            // 每行单独切列(整块切列会互相覆盖)。
            let row_rect = Rect::new(rows_area.x, rows_area.y + row_idx as u16, rows_area.width, 1);
            let cells: [Rect; 6] = Layout::horizontal(columns).areas(row_rect);
            let values = [
                if rule.enabled { "●".to_string() } else { "○".to_string() },
                rule.name.clone(),
                rule.protocol.label().to_string(),
                format!(":{}", rule.local_port),
                format!("{}:{}", rule.target, rule.remote_port),
                if rule.enabled { "启用" } else { "停用" }.to_string(),
            ];
            for (col_idx, value) in values.iter().enumerate() {
                let mut style = if rule.enabled {
                    Style::new().green()
                } else {
                    Style::new().dark_gray()
                };
                if is_selected {
                    style = style.bg(Color::DarkGray).bold();
                }
                Paragraph::new(Line::from(format!(" {value}")).style(style))
                    .render(cells[col_idx], buf);
            }
        }
        if self.rules.is_empty() {
            Paragraph::new(
                Line::from("还没有规则,按 a 新增一条".dark_gray()).centered(),
            )
            .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
            .render(rows_area, buf);
        }
    }
}

/// 定高的追加规则表单,活动字段高亮。
#[derive(Clone)]
struct FormWidget {
    fields: Vec<(String, String, bool)>,
    error: String,
}

impl Widget for FormWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let block = Block::bordered()
            .title(Line::from(" 新增规则 ").cyan().bold())
            .border_style(Style::new().cyan());
        let inner = block.inner(area);
        block.render(area, buf);

        let rows: [Rect; 5] = Layout::vertical([Constraint::Length(1); 5]).areas(inner);
        for ((label, value, active), row) in self.fields.iter().zip(rows.iter()) {
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
            .style(style)
            .render(*row, buf);
        }
        if !self.error.is_empty() {
            Paragraph::new(Line::from(self.error).red()).render(inner, buf);
        }
    }
}

/// 底部键位说明。
#[derive(Clone)]
struct HelpWidget;

impl Widget for HelpWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        Paragraph::new(
            Line::from(
                " ↑/↓ 选择   空格 启/停   a 新增   d 删除   c 连接/断开   Esc 设备   q 退出 "
                    .dark_gray(),
            )
            .centered(),
        )
        .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
        .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_rows_mark_single_active_field() {
        let rows = build_form_rows(2, "web", Protocol::Tcp, "8080", "10.0.0.2", "80");
        assert_eq!(rows.len(), FORM_FIELDS.len());
        assert_eq!(
            rows.iter().filter(|(_, _, a)| *a).count(),
            1,
            "exactly one active field"
        );
        assert!(rows[2].2);
    }
}
