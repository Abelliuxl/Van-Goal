mod state;
mod ui;

use gpui::prelude::*;
use gpui::{
    actions, point, px, size, App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBounds,
    WindowOptions,
};
use state::AppState;
use ui::root::RootView;
use ui::settings_window::OpenSettings;
use van_goal_core::log_debug;

actions!(van_goal, [NewSession, RefreshSessions, ToggleSidebar, Quit]);

struct StateGlobal(gpui::Entity<AppState>);
impl gpui::Global for StateGlobal {}

struct MainWindowGlobal(Option<gpui::WindowHandle<RootView>>);
impl gpui::Global for MainWindowGlobal {}

struct SettingsWindowGlobal(Option<gpui::WindowHandle<ui::settings_window::SettingsView>>);
impl gpui::Global for SettingsWindowGlobal {}

/// Window size for a first launch, before anything has been remembered.
const DEFAULT_WINDOW_WIDTH: f32 = 1180.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 760.0;

/// Where to open the window: the geometry from last time when it still makes
/// sense, otherwise centred at the default size.
fn initial_window_bounds(state: &gpui::Entity<AppState>, cx: &mut App) -> WindowBounds {
    let saved = state.read(cx).settings.window.filter(|s| s.is_plausible());
    match saved {
        Some(saved) => {
            let bounds = Bounds {
                origin: point(px(saved.x), px(saved.y)),
                size: size(px(saved.width), px(saved.height)),
            };
            if saved.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }
        }
        None => WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
            cx,
        )),
    }
}

fn open_main_window(
    state: gpui::Entity<AppState>,
    cx: &mut App,
) -> gpui::Result<gpui::WindowHandle<RootView>> {
    let bounds = initial_window_bounds(&state, cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(bounds),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Van-Goal".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(12.0))),
            }),
            window_min_size: Some(size(px(560.0), px(480.0))),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| RootView::new(state, window, cx)),
    )
}

fn main() {
    // Dedicated async runtime for backend transports (HTTP, WS, SSE, CLIs).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    // Clicking the Dock icon brings the main window back after the user
    // closed it with the red traffic light.
    let app = Application::new();
    app.on_reopen(|cx| {
        cx.activate(true);
        let Some(state) = cx.try_global::<StateGlobal>().map(|g| g.0.clone()) else {
            return;
        };
        let existing = cx
            .try_global::<MainWindowGlobal>()
            .and_then(|global| global.0.clone());
        let activated = existing
            .map(|handle| {
                handle
                    .update(cx, |_root, window, _cx| window.activate_window())
                    .is_ok()
            })
            .unwrap_or(false);
        if !activated {
            match open_main_window(state, cx) {
                Ok(handle) => {
                    let _ = handle.update(cx, |_root, window, _cx| window.activate_window());
                    cx.set_global(MainWindowGlobal(Some(handle)));
                }
                Err(error) => {
                    log_debug!("app", "failed to reopen main window: {error}");
                }
            }
        }
    });
    app.run(move |cx: &mut App| {
        cx.set_global(state::TokioGlobal(runtime));
        ui::editor::bind_editor_keys(cx);
        ui::chat::bind_chat_keys(cx);

        let state = cx.new(|cx| AppState::new(cx));
        cx.set_global(StateGlobal(state.clone()));

        match open_main_window(state, cx) {
            Ok(window) => {
                cx.set_global(MainWindowGlobal(Some(window)));
                let _ = window.update(cx, |_root, window, _cx| window.activate_window());
            }
            Err(error) => {
                log_debug!("app", "failed to open main window: {error}");
            }
        }

        let state = cx.global::<StateGlobal>().0.clone();
        state.update(cx, |state, cx| state.bootstrap(cx));

        // A managed server whose backend is scoped by a workspace does not
        // outlive the app: its project is fixed when it starts, so a server left
        // behind pins the chat to a directory the app can no longer change, and
        // the next launch would reuse it and quietly ignore the setting. Hermes
        // is not scoped that way and its lifetime is left as it has always been.
        cx.on_app_quit({
            let state = cx.global::<StateGlobal>().0.clone();
            move |cx| {
                let server = state.read(cx).local_server.clone();
                if server
                    .managed_server()
                    .is_some_and(|server| server.uses_workspace())
                {
                    // Signalled here, not in the future below: quitting gives a
                    // handler a moment, and a signal needs none.
                    server.stop();
                }
                async {}
            }
        })
        .detach();

        // App-level actions.
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &NewSession, cx| {
            let state = cx.global::<StateGlobal>().0.clone();
            state.update(cx, |state, cx| state.start_fresh_chat(cx));
        });
        cx.on_action(|_: &RefreshSessions, cx| {
            let state = cx.global::<StateGlobal>().0.clone();
            state.update(cx, |state, cx| state.refresh_sessions(true, cx));
        });
        cx.on_action(|_: &ToggleSidebar, cx| {
            let window = cx
                .try_global::<MainWindowGlobal>()
                .and_then(|global| global.0.clone());
            if let Some(window) = window {
                let _ = window.update(cx, |root, _window, cx| root.toggle_sidebar(cx));
            }
        });
        cx.on_action(|_: &OpenSettings, cx| {
            let existing = cx
                .try_global::<SettingsWindowGlobal>()
                .and_then(|global| global.0.clone());
            let activated = existing
                .map(|handle| {
                    handle
                        .update(cx, |_view, window, _cx| window.activate_window())
                        .is_ok()
                })
                .unwrap_or(false);
            if !activated {
                let state = cx.global::<StateGlobal>().0.clone();
                match ui::settings_window::SettingsView::open(state, cx) {
                    Ok(handle) => {
                        let _ = handle.update(cx, |_view, window, _cx| {
                            window.activate_window();
                        });
                        cx.set_global(SettingsWindowGlobal(Some(handle)));
                    }
                    Err(error) => {
                        log_debug!("app", "failed to open settings window: {error}");
                    }
                }
            }
        });

        // Keyboard shortcuts for the app actions.
        cx.bind_keys([
            KeyBinding::new("cmd-n", NewSession, None),
            KeyBinding::new("cmd-r", RefreshSessions, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        // Native menu bar.
        cx.set_menus(vec![
            Menu {
                name: "Van-Goal".into(),
                items: vec![
                    MenuItem::action("About Van-Goal", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Settings", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Van-Goal", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("New Session", NewSession)],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Sidebar", ToggleSidebar),
                    MenuItem::action("Refresh Sessions", RefreshSessions),
                ],
            },
        ]);

        cx.activate(true);
    });
}
