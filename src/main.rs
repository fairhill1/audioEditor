mod app;
mod audio;
mod commands;
mod input;
mod modal;
mod playback;
mod project;
mod render;
mod timeline;
mod undo;

use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::App;

/// Set up a macOS Edit menu with standard text-editing key equivalents so that
/// Cmd+C/V/X/A work inside native panels (e.g. the file-open dialog search field).
/// When the winit window is key, its view claims key-equivalent events first, so
/// these menu items don't interfere with the app's own shortcut handling.
#[cfg(target_os = "macos")]
pub(crate) fn setup_macos_edit_menu() {
    use objc2::sel;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;

    let mtm = MainThreadMarker::new().expect("must be on main thread");

    let app = NSApplication::sharedApplication(mtm);

    let menu_bar = app.mainMenu().unwrap_or_else(|| {
        let m = NSMenu::new(mtm);
        app.setMainMenu(Some(&m));
        m
    });

    let edit_item = NSMenuItem::new(mtm);
    let edit_title = NSString::from_str("Edit");
    let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &edit_title);

    for (title, action, key) in [
        ("Cut", sel!(cut:), "x"),
        ("Copy", sel!(copy:), "c"),
        ("Paste", sel!(paste:), "v"),
        ("Select All", sel!(selectAll:), "a"),
    ] {
        let t = NSString::from_str(title);
        let k = NSString::from_str(key);
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &t, Some(action), &k)
        };
        edit_menu.addItem(&item);
    }

    edit_item.setSubmenu(Some(&edit_menu));
    menu_bar.addItem(&edit_item);
}

fn main() {
    env_logger::init();
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}
