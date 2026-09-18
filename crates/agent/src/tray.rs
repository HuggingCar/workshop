//! System tray, so closing the window leaves the agent printing.
//!
//! Same behaviour as the Qt agent: the menu offers "Otwórz" and "Zakończ", and clicking the
//! icon reopens the window.
//!
//! Linux speaks the StatusNotifierItem protocol directly (`ksni`) instead of going through
//! libayatana-appindicator: the GTK 3 stack that backend drags into an egui process measured
//! 88 MB of resident memory, more than the whole rest of the agent.

use std::sync::mpsc::{Receiver, Sender, channel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Quit,
}

/// Pixels of the embedded app icon, and its square side.
fn pixels() -> (Vec<u8>, u32) {
    let source = std::io::Cursor::new(include_bytes!("../assets/icon.png").as_slice());
    let mut reader = png::Decoder::new(source)
        .read_info()
        .expect("icon.png header");
    let mut pixels = vec![0; reader.output_buffer_size().expect("icon.png size")];
    let info = reader.next_frame(&mut pixels).expect("icon.png pixels");
    pixels.truncate(info.buffer_size());
    (pixels, info.width)
}

pub fn window_icon() -> egui::IconData {
    let (rgba, side) = pixels();
    egui::IconData {
        rgba,
        width: side,
        height: side,
    }
}

#[cfg(target_os = "linux")]
pub use linux::{Tray, spawn};

#[cfg(target_os = "linux")]
mod linux {
    use super::{Command, Sender, channel, pixels};

    pub struct Tray {
        pub commands: super::Receiver<Command>,
        /// Dropping the handle removes the icon, so the window owns it for the run.
        _handle: ksni::blocking::Handle<Agent>,
    }

    struct Agent {
        commands: Sender<Command>,
        context: egui::Context,
    }

    impl Agent {
        /// Wakes the window's event loop even while it is hidden and idle.
        fn send(&self, command: Command) {
            self.commands.send(command).ok();
            self.context.request_repaint();
        }
    }

    impl ksni::Tray for Agent {
        fn id(&self) -> String {
            "huggingcar-agent".into()
        }

        fn title(&self) -> String {
            "HuggingCar Agent".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            let (rgba, side) = pixels();
            // StatusNotifierItem wants ARGB32 in network byte order.
            let data = rgba
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
                .collect();
            vec![ksni::Icon {
                width: side as i32,
                height: side as i32,
                data,
            }]
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(Command::Open);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            vec![
                ksni::menu::StandardItem {
                    label: "Otwórz".into(),
                    activate: Box::new(|this: &mut Self| this.send(Command::Open)),
                    ..Default::default()
                }
                .into(),
                ksni::menu::StandardItem {
                    label: "Zakończ".into(),
                    activate: Box::new(|this: &mut Self| this.send(Command::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// `None` when the desktop has no tray; the window then keeps its own Zakończ button.
    pub fn spawn(context: egui::Context) -> Option<Tray> {
        use ksni::blocking::TrayMethods;

        let (sender, commands) = channel();
        let handle = Agent {
            commands: sender,
            context,
        }
        .spawn()
        .ok()?;
        Some(Tray {
            commands,
            _handle: handle,
        })
    }
}

#[cfg(not(target_os = "linux"))]
pub use other::{Tray, spawn};

#[cfg(not(target_os = "linux"))]
mod other {
    use tray_icon::{
        Icon, MouseButton, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem},
    };

    use super::{Command, Receiver, Sender, channel, pixels};

    pub struct Tray {
        pub commands: Receiver<Command>,
        /// Dropping the icon removes it from the tray, so the window owns it for the run.
        _icon: tray_icon::TrayIcon,
    }

    /// Forward tray activity to the window, waking its event loop even while it is hidden.
    fn forward(context: egui::Context, sender: Sender<Command>) {
        let menu_sender = sender.clone();
        let menu_context = context.clone();
        std::thread::spawn(move || {
            while let Ok(event) = MenuEvent::receiver().recv() {
                let command = match event.id.as_ref() {
                    "quit" => Command::Quit,
                    _ => Command::Open,
                };
                if menu_sender.send(command).is_err() {
                    return;
                }
                menu_context.request_repaint();
            }
        });
        std::thread::spawn(move || {
            while let Ok(event) = TrayIconEvent::receiver().recv() {
                let clicked = matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                );
                if clicked {
                    if sender.send(Command::Open).is_err() {
                        return;
                    }
                    context.request_repaint();
                }
            }
        });
    }

    /// Windows and macOS require the tray on the thread running the event loop.
    pub fn spawn(context: egui::Context) -> Option<Tray> {
        let (rgba, side) = pixels();
        let menu = Menu::new();
        menu.append(&MenuItem::with_id("open", "Otwórz", true, None))
            .ok()?;
        menu.append(&MenuItem::with_id("quit", "Zakończ", true, None))
            .ok()?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("HuggingCar Agent")
            .with_icon(Icon::from_rgba(rgba, side, side).ok()?)
            .build()
            .ok()?;
        let (sender, commands) = channel();
        forward(context, sender);
        Some(Tray {
            commands,
            _icon: icon,
        })
    }
}
