//! Login page: render the QR to scan and poll its status. On success the
//! parent's page handle flips to the port-map page.

use crate::app::Page;
use crate::backend;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Flex, Layout, Rect},
        style::{Style, Stylize},
        text::Line,
        widgets::{Block, Paragraph, Widget},
    },
};

#[derive(Default, Props)]
pub struct LoginProps {
    /// Parent's page state; login success writes `Page::PortMap` into it.
    /// `Option` mirrors the framework's own props (handles have no Default).
    pub page: Option<State<Page>>,
}

#[component]
pub fn Login(props: &mut LoginProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut page = props.page.clone().expect("Login 组件需要 page 属性");
    let mut session = hooks.use_state(|| None::<backend::QrLoginSession>);
    let mut status = hooks.use_state(|| "正在生成登录二维码…".to_string());

    // 先尝试恢复保存的登录态(token 文件),失败才走扫码;之后创建
    // 二维码并轮询,过期/被取消时自动重新生成。
    hooks.use_future(async move {
        loop {
            match backend::restore_saved_session().await {
                backend::RestoreOutcome::Restored => {
                    status.set("已从保存的登录态恢复".green().to_string());
                    page.set(Page::Devices);
                    return;
                }
                backend::RestoreOutcome::NoSession
                | backend::RestoreOutcome::InvalidCredentials => {}
                backend::RestoreOutcome::Failed(message) => {
                    status.set(format!("无法验证保存的登录态({message}),请重新扫码"));
                }
            }
            match backend::create_qr_login().await {
                Ok(new_session) => {
                    session.set(Some(new_session));
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                        let Some(current) = session.read().clone() else {
                            break;
                        };
                        match backend::query_qr_login(&current.key).await {
                            Ok(backend::QrStatus::WaitingScan) => {
                                status.set("等待扫码:打开 UU 远程 App → 扫一扫".into());
                            }
                            Ok(backend::QrStatus::WaitingConfirm) => {
                                status.set("已扫码,请在手机上确认登录".green().to_string());
                            }
                            Ok(backend::QrStatus::Success(token)) => {
                                status.set(format!("登录成功:{token}"));
                                page.set(Page::Devices);
                                return;
                            }
                            Ok(backend::QrStatus::Canceled) => {
                                status.set("手机端已取消,正在刷新二维码…".into());
                                break;
                            }
                            Ok(backend::QrStatus::Expired) | Err(_) => {
                                status.set("二维码已过期,正在重新生成…".into());
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    status.set(format!("登录服务不可用({e}),5 秒后重试"));
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
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
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                exit();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let qr_lines = session
        .read().clone()
        .as_ref()
        .map(|s| render_qr(&s.url))
        .unwrap_or_default();
    let status_text = status.read().clone();

    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Percentage(100),
        height: Constraint::Percentage(100),
    ) {
        widget(LoginWidget { qr_lines, status: status_text })
    })
}

/// Encode `text` as a QR image using half-block characters (two module rows
/// per terminal row). `Light`/`Dark` are swapped because terminals draw
/// light text on a dark background.
fn render_qr(text: &str) -> Vec<Line<'static>> {
    use qrcode::render::unicode;
    let code = qrcode::QrCode::new(text.as_bytes()).expect("登录链接生成二维码失败");
    code.render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(false)
        .build()
        .lines()
        .map(|l| Line::from(l.to_string()).style(Style::new().cyan()))
        .collect()
}

/// Centered card: QR on top, status and hints underneath.
#[derive(Clone)]
struct LoginWidget {
    qr_lines: Vec<Line<'static>>,
    status: String,
}

impl Widget for LoginWidget {
    fn render(self, area: Rect, buf: &mut ratatui_kit::ratatui::buffer::Buffer) {
        let block = Block::bordered()
            .title(Line::from(" uunet 登录 ").cyan().bold())
            .border_style(Style::new().cyan());
        let inner = block.inner(area);
        block.render(area, buf);

        // Title / status / QR / hint, the group centered in the card.
        let qr_height = self.qr_lines.len() as u16;
        let chunks: [Rect; 4] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(qr_height.max(1)),
            Constraint::Length(1),
        ])
        .flex(Flex::Center)
        .areas(inner);

        Paragraph::new(Line::from("扫码登录 uunet").bold().centered())
            .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
            .render(chunks[0], buf);
        Paragraph::new(Line::from(self.status).centered())
            .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
            .render(chunks[1], buf);
        if !self.qr_lines.is_empty() {
            Paragraph::new(self.qr_lines)
                .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
                .render(chunks[2], buf);
        }
        let hint = if crate::backend::mock_mode() {
            "mock 模式:二维码会自动走完 扫码 → 确认 → 登录成功"
        } else {
            "打开 UU 远程手机端「扫一扫」完成登录"
        };
        Paragraph::new(Line::from(hint.dark_gray()).centered())
        .alignment(ratatui_kit::ratatui::layout::Alignment::Center)
        .render(chunks[3], buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_has_square_layout() {
        let lines = render_qr("https://uunet.example/qr/test");
        let width = lines[0].width();
        assert!(width > 10, "qr too narrow: {width}");
        assert_eq!(
            lines.iter().map(|l| l.width()).collect::<Vec<_>>(),
            vec![width; lines.len()],
            "qr rows must share one width"
        );
        // Dense1x2 packs two module rows into one character row.
        assert!(lines.len() * 2 >= width / 2, "qr looks stretched");
    }
}
