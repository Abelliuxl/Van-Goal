//! Windows system-tray integration for the desktop client.
//!
//! GPUI owns the main window and its message loop. The tray icon is created on
//! that same thread, while tray menu callbacks send small commands through a
//! channel that the GPUI app polls. This keeps all tray code Windows-only and
//! leaves the core, macOS, and mobile clients unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Global, Window};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindowAsync, SW_HIDE, SW_SHOW,
};

const MAIN_WINDOW_TITLE: &str = "Van-Goal";
const TRAY_GUID: u128 = 0x9f7b_3d18_4f7a_4b87_9a54_6b2d_9e7f_3c11;

#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    Show,
    Quit,
}

pub struct WindowsTrayGlobal {
    _icon: TrayIcon,
    should_quit: Arc<AtomicBool>,
}

impl Global for WindowsTrayGlobal {}

impl WindowsTrayGlobal {
    pub fn create() -> anyhow::Result<(Self, Receiver<TrayCommand>)> {
        let (sender, receiver) = mpsc::channel();
        let show_item = MenuItem::new("显示 Van-Goal", true, None);
        let quit_item = MenuItem::new("退出 Van-Goal", true, None);
        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();
        let menu = Menu::new();
        menu.append(&show_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit_item)?;

        let menu_sender = sender.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = if event.id() == &show_id {
                Some(TrayCommand::Show)
            } else if event.id() == &quit_id {
                Some(TrayCommand::Quit)
            } else {
                None
            };
            if let Some(command) = command {
                let _ = menu_sender.send(command);
            }
        }));

        let click_sender = sender;
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                let _ = click_sender.send(TrayCommand::Show);
            }
        }));

        let icon = Icon::from_resource(1, Some((16, 16)))?;
        let tray_icon = TrayIconBuilder::new()
            .with_guid(TRAY_GUID)
            .with_menu(Box::new(menu))
            .with_tooltip("Van-Goal")
            .with_icon(icon)
            .build()?;

        Ok((
            Self {
                _icon: tray_icon,
                should_quit: Arc::new(AtomicBool::new(false)),
            },
            receiver,
        ))
    }

    pub fn request_quit(&self) {
        self.should_quit.store(true, Ordering::Release);
    }

    fn is_quitting(&self) -> bool {
        self.should_quit.load(Ordering::Acquire)
    }
}

/// Closing the main window hides it in the tray. An explicit Quit action sets
/// `should_quit`, allowing GPUI to close the process normally.
pub fn install_close_handler(window: &mut Window, cx: &App) {
    window.on_window_should_close(cx, |_window, cx| {
        let Some(tray) = cx.try_global::<WindowsTrayGlobal>() else {
            return true;
        };
        if tray.is_quitting() {
            return true;
        }
        hide_main_window();
        false
    });
}

pub fn start_command_pump(cx: &mut App, receiver: Receiver<TrayCommand>) {
    cx.spawn(async move |cx| loop {
        cx.background_executor()
            .timer(Duration::from_millis(80))
            .await;

        while let Ok(command) = receiver.try_recv() {
            let should_continue = cx
                .update(|cx| match command {
                    TrayCommand::Show => {
                        show_main_window();
                        if let Some(handle) = cx
                            .try_global::<crate::MainWindowGlobal>()
                            .and_then(|global| global.0.clone())
                        {
                            let _ = handle.update(cx, |_root, window, _cx| {
                                window.activate_window();
                            });
                        }
                        true
                    }
                    TrayCommand::Quit => {
                        if let Some(tray) = cx.try_global::<WindowsTrayGlobal>() {
                            tray.request_quit();
                        }
                        cx.quit();
                        false
                    }
                })
                .unwrap_or(false);

            if !should_continue {
                return;
            }
        }
    })
    .detach();
}

fn main_window_handle() -> windows_sys::Win32::Foundation::HWND {
    let mut title: Vec<u16> = MAIN_WINDOW_TITLE.encode_utf16().collect();
    title.push(0);
    unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) }
}

pub fn hide_main_window() {
    let hwnd = main_window_handle();
    if !hwnd.is_null() {
        unsafe {
            ShowWindowAsync(hwnd, SW_HIDE);
        }
    }
}

fn show_main_window() {
    let hwnd = main_window_handle();
    if !hwnd.is_null() {
        unsafe {
            ShowWindowAsync(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
    }
}
