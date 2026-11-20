//! Port-map page: rule list with selection, an add-rule form, and a
//! delete confirmation. All backend calls go through `crate::backend`.

use crate::app::Page;
use crate::backend::{self, Protocol, Rule, RuleForm};
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
    /// Parent's page state; Esc writes `Page::Devices` into it.
    pub page: Option<State<Page>>,
    /// The device whose rules this page manages.
    pub device_id: Option<String>,
}

#[component]
pub fn PortMap(props: &mut PortMapProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let page = props.page.clone().expect("PortMap 组件需要 page 属性");
    let device_id = props
        .device_id
        .clone()
        .expect("PortMap 组件需要 device_id 属性");
    let device = backend::device_by_id(&device_id);
    let device_id_for_load = device_id.clone();
    let device_id_for_keys = device_id.clone();
    let device_id_for_modal = device_id.clone();
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

    // Initial load; later reloads happen inline in the key handler.
    let mut load_rules = rules.clone();
    hooks.use_future(async move {
        load_rules.set(backend::list_rules(&device_id_for_load));
    });

    let mut exit = hooks.use_exit();
    let mut rules_for_modal = rules.clone();
    let mut page_for_esc = page.clone();
    let mut selected_for_modal = selected.clone();
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        // While the delete modal is up, the built-in ConfirmModal owns the
        // keyboard; stay out of its way.
        if confirm_id.get().is_some() {
            return EventResult::Ignored;
        }

        if form_open.get() {
            return handle_form_key(
                key.code,
                device_id_for_keys.clone(),
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
                page_for_esc.set(Page::Devices);
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
                    backend::toggle_rule(rule.id);
                    rules.set(backend::list_rules(&device_id));
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
            KeyCode::Char('r') => {
                let fresh = backend::list_rules(&device_id);
                selected.set(min(selected.get(), fresh.len().saturating_sub(1)));
                rules.set(fresh);
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
    let confirm_rule = confirm_id
        .get()
        .and_then(|id| rules_value.iter().find(|r| r.id == id))
        .cloned();

    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Percentage(100),
        height: Constraint::Percentage(100),
    ) {
        widget(HeaderWidget { title: header_title, total: rules_value.len(), enabled: rules_value.iter().filter(|r| r.enabled).count() })
        widget(RulesWidget { rules: rules_value.clone(), selected: selected_value, reserved_bottom: if show_form { FORM_HEIGHT } else { 0 } })
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
                    backend::delete_rule(rule.id);
                    confirm_id.set(None);
                    let fresh = backend::list_rules(&device_id_for_modal);
                    selected_for_modal.set(min(selected_for_modal.get(), fresh.len().saturating_sub(1)));
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

/// Form keys: Up/Down move fields, Enter advances or submits, Esc closes,
/// typing edits the active field (`t` on the protocol field flips TCP/UDP).
#[allow(clippy::too_many_arguments)]
fn handle_form_key(
    code: KeyCode,
    device_id: String,
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
    match code {
        KeyCode::Esc => {
            form_open.set(false);
            EventResult::Consumed
        }
        KeyCode::Up => {
            let idx = field_idx.get();
            field_idx.set(if idx == 0 { FORM_FIELDS.len() - 1 } else { idx - 1 });
            EventResult::Consumed
        }
        KeyCode::Down | KeyCode::Tab => {
            field_idx.set((field_idx.get() + 1) % FORM_FIELDS.len());
            EventResult::Consumed
        }
        KeyCode::Enter => {
            if field_idx.get() + 1 < FORM_FIELDS.len() {
                field_idx.set(field_idx.get() + 1);
                EventResult::Consumed
            } else {
                submit_form(device_id, f_name, f_proto, f_local, f_target, f_port, form_error, form_open, rules, selected)
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
            let mut value = input_field(idx, f_name, f_local, f_target, f_port).read().clone();
            value.pop();
            set_input_field(idx, value, f_name, f_local, f_target, f_port);
            EventResult::Consumed
        }
        KeyCode::Char(c) => {
            let idx = field_idx.get();
            if idx == 1 {
                // The protocol field toggles instead of accepting text.
                f_proto.set(match f_proto.get() {
                    Protocol::Tcp => Protocol::Udp,
                    Protocol::Udp => Protocol::Tcp,
                });
                return EventResult::Consumed;
            }
            let mut value = input_field(idx, f_name, f_local, f_target, f_port).read().clone();
            if value.chars().count() < 32 {
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
    device_id: String,
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
    let form = RuleForm {
        name: f_name.read().clone(),
        protocol: f_proto.get(),
        local_port: f_local.read().parse().unwrap_or(0),
        target: f_target.read().clone(),
        target_port: f_port.read().parse().unwrap_or(0),
    };
    match backend::add_rule(&device_id, &form) {
        Ok(_) => {
            form_open.set(false);
            let fresh = backend::list_rules(&device_id);
            selected.set(fresh.len().saturating_sub(1));
            rules.set(fresh);
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

/// Two-line header: page title with rule counts.
#[derive(Clone)]
struct HeaderWidget {
    title: String,
    total: usize,
    enabled: usize,
}

impl Widget for HeaderWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        Paragraph::new(Line::from(format!(
            " {}   规则 {} 条,运行 {} 条",
            self.title, self.total, self.enabled
        ))
        .cyan()
        .bold())
        .render(area, buf);
    }
}

/// Bordered rules table with a column header row; rows past the visible
/// area scroll by page as the selection moves.
#[derive(Clone)]
struct RulesWidget {
    rules: Vec<Rule>,
    selected: usize,
    /// Rows reserved at the bottom for the add form (0 = none).
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
        let visible = inner.height.saturating_sub(1) as usize; // minus column header
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
            // One terminal row per rule; column rects must be cut from that
            // single row, not from the whole rows area (rows would overwrite
            // each other at the top).
            let row_rect = Rect::new(rows_area.x, rows_area.y + row_idx as u16, rows_area.width, 1);
            let cells: [Rect; 6] = Layout::horizontal(columns).areas(row_rect);
            let real_idx = skip + row_idx;
            let is_selected = real_idx == self.selected;
            let values = [
                if rule.enabled { "●".to_string() } else { "○".to_string() },
                rule.name.clone(),
                rule.protocol.label().to_string(),
                rule.listen_label(),
                rule.target_label(),
                rule.status_label().to_string(),
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
            Paragraph::new(Line::from("还没有规则,按 a 新增一条".dark_gray()).centered())
                .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
                .render(rows_area, buf);
        }
    }
}

/// Fixed-height add-rule form with the active field highlighted.
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
            Paragraph::new(Line::from(format!("{marker}{label:<6} {value}{cursor}")).style(style))
                .render(*row, buf);
        }
        if !self.error.is_empty() {
            Paragraph::new(Line::from(self.error).red()).render(inner, buf);
        }
    }
}

/// One-line key help pinned to the bottom.
#[derive(Clone)]
struct HelpWidget;

impl Widget for HelpWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        Paragraph::new(
            Line::from(" ↑/↓ 选择   空格 启/停   a 新增   d 删除   r 刷新   Esc 设备列表   q 退出 ".dark_gray())
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
    fn rules_widget_renders_header_and_rows() {
        // Fixed input instead of the process-global mock store: tests run in
        // parallel and share that store, so global data would race.
        let rules = vec![
            Rule {
                id: 1,
                name: "NAS Web".into(),
                protocol: Protocol::Tcp,
                local_port: 8080,
                target: "10.0.0.2".into(),
                target_port: 5000,
                enabled: true,
            },
        ];
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        RulesWidget {
            rules: rules,
            selected: 0,
            reserved_bottom: 0,
        }
        .render(area, &mut buf);

        let rows: Vec<String> = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        // Wide (CJK) characters occupy two buffer cells; collapse spaces so
        // labels can be matched regardless of column packing.
        let joined = rows.join("\n").replace(' ', "");
        for label in ["名称", "协议", "本地监听", "NASWeb", ":8080/tcp", "运行中"] {
            assert!(joined.contains(label), "missing {label} in:\n{joined}");
        }
        // borders intact
        assert!(rows[0].starts_with('┌') && rows[0].ends_with('┐'));
    }

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
