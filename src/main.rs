mod about;
mod attach_dialog;
mod blast;
mod editor;
mod feature_dialog;
mod genetic_code;
mod history;
mod hotkeys;
mod layout;
mod locks;
mod map;
mod map_layout;
mod model;
mod panes;
mod pgx;
mod sbol;
mod settings;
mod tasks;
mod snapgene;
mod structure;
mod structure_render;
mod structure_view;
mod terminal;
mod text_input;
mod theme;
mod workspace;

use std::path::PathBuf;

use gpui::{px, size, App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions};

use workspace::Workspace;

fn main() {
    // Usage: plasmid-gpui [folder] [file-to-open]
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("current dir"));
    let root = root.canonicalize().unwrap_or(root);
    let initial = args.next().map(PathBuf::from);

    Application::new().run(move |cx: &mut App| {
        tasks::init(cx);
        workspace::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(1400.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Plasmid Editor (GPUI PoC)".into()), ..Default::default() }),
                app_id: Some("plasmid-gpui".into()),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let mut ws = Workspace::new(root.clone(), window, cx);
                    if let Some(path) = &initial {
                        ws.open(path, window, cx);
                    }
                    ws
                })
            },
        )
        .expect("open window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
