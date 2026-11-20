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

    let qr = session
        .read().clone()
        .as_ref()
        .map(|s| qr_matrix(&s.url));
    let status_text = status.read().clone();

    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Percentage(100),
        height: Constraint::Percentage(100),
    ) {
        widget(LoginWidget { qr, status: status_text })
    })
}

/// 把文本编码为二维码模块矩阵:返回 (含白边的宽度, 行优先的暗模块表)。
/// 外圈补 2 模块白边(标准静区),白色背景由渲染时统一铺。
fn qr_matrix(text: &str) -> (usize, Vec<bool>) {
    use qrcode::EcLevel;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), EcLevel::L)
        .expect("登录链接生成二维码失败");
    let size = code.width();
    let quiet = 2;
    let width = size + quiet * 2;
    let mut modules = vec![false; width * width]; // false = 白
    for (index, color) in code.to_colors().iter().enumerate() {
        let x = index % size;
        let y = index / size;
        if *color == qrcode::Color::Dark {
            modules[(y + quiet) * width + (x + quiet)] = true;
        }
    }
    (width, modules)
}

/// Centered card: QR on top, status and hints underneath.
#[derive(Clone)]
struct LoginWidget {
    /// (含白边宽度, 暗模块表);None 表示尚未生成。
    qr: Option<(usize, Vec<bool>)>,
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
        // 二维码:每模块 1 列宽、半个字符行高(半块字符)。
        let qr_cols = self.qr.as_ref().map(|(width, _)| *width as u16).unwrap_or(0);
        let qr_rows = qr_cols.div_ceil(2);
        let chunks: [Rect; 4] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(qr_rows.max(1)),
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
        if let Some((width, modules)) = &self.qr {
            draw_qr(buf, chunks[2], *width, modules);
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

/// 白底黑块直绘:整块区域铺白,暗模块以黑色半块字符呈现
/// (一个终端字符行承载两行模块)。
fn draw_qr(buf: &mut ratatui_kit::ratatui::buffer::Buffer, area: Rect, width: usize, modules: &[bool]) {
    let style = Style::new().fg(ratatui_kit::ratatui::style::Color::Black)
        .bg(ratatui_kit::ratatui::style::Color::White);
    let dark = |x: usize, y: usize| -> bool {
        x < width && y < width && modules[y * width + x]
    };
    for row in 0..area.height as usize {
        let top = row * 2;
        for col in 0..(area.width as usize).min(width) {
            let upper = dark(col, top);
            let lower = dark(col, top + 1);
            let symbol = match (upper, lower) {
                (true, true) => "\u{2588}",   // █ 全黑
                (true, false) => "\u{2580}",  // ▀ 上黑
                (false, true) => "\u{2584}",  // ▄ 下黑
                (false, false) => " ",        // 白底
            };
            let cell = &mut buf[(area.x + col as u16, area.y + row as u16)];
            cell.set_symbol(symbol);
            cell.set_style(style);
        }
        // 右侧若因宽度不足有富余,同样铺白保持方形背景。
        for col in (area.width as usize).min(width)..area.width as usize {
            let cell = &mut buf[(area.x + col as u16, area.y + row as u16)];
            cell.set_symbol(" ");
            cell.set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_matrix_has_quiet_zone_and_square_shape() {
        let (width, modules) = qr_matrix("https://uunet.example/qr/test");
        assert!(width > 10, "qr too narrow: {width}");
        assert_eq!(modules.len(), width * width, "matrix must be square");
        // 白边静区:四角 2x2 全为白。
        for (x, y) in [(0, 0), (1, 1), (width - 1, width - 1), (width - 2, width - 2)] {
            assert!(!modules[y * width + x], "quiet zone at ({x},{y}) must be white");
        }
        // 码区存在暗模块。
        assert!(modules.iter().any(|dark| *dark), "qr has no dark modules");
    }
}
