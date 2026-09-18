//! Setup window for the workshop PC: enter the API address, token and printer port,
//! then leave the agent running.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel},
    },
    thread::JoinHandle,
    time::Duration,
};

use eframe::egui;
use workshop_agent::{Config, Error, Notify, Result, build, state_dir};

mod tray;

const BAUDRATES: [u32; 5] = [9600, 19200, 38400, 57600, 115_200];

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Błąd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (data, headless_mode) = options(std::env::args_os().skip(1))?;
    if headless_mode {
        headless(&data)?;
        return Ok(());
    }
    eframe::run_native(
        "Drukarka fiskalna",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([620.0, 380.0])
                .with_icon(tray::window_icon()),
            ..Default::default()
        },
        Box::new(move |context| {
            Ok(Box::new(App::new(
                data,
                tray::spawn(context.egui_ctx.clone()),
            )))
        }),
    )?;
    Ok(())
}

fn options(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(PathBuf, bool)> {
    let mut data = None;
    let mut headless = false;
    while let Some(argument) = args.next() {
        if argument == "--headless" {
            headless = true;
        } else if argument == "--data-dir" {
            data = Some(PathBuf::from(
                args.next()
                    .filter(|path| !path.is_empty())
                    .ok_or_else(|| Error::value("--data-dir wymaga ścieżki katalogu."))?,
            ));
        } else {
            return Err(Error::value(format!(
                "Nieznany argument: {}",
                argument.to_string_lossy()
            )));
        }
    }
    Ok((
        match data {
            Some(path) => path,
            None => state_dir()?,
        },
        headless,
    ))
}

/// Service shutdown waits for the current job, including its server report and journal.
fn headless(data: &std::path::Path) -> Result<()> {
    #[cfg(windows)]
    attach_parent_console()
        .map_err(|error| Error::value(format!("Nie można dołączyć do konsoli: {error}")))?;
    let stopping = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopping);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
        .map_err(|error| Error::value(format!("Nie można obsłużyć zatrzymania: {error}")))?;
    let config = Config::load(&data.join("fiscal.json"));
    let notify: Notify = Arc::new(|message: &str, level: &str| println!("[{level:>7}] {message}"));
    run_worker(config, data.to_path_buf(), stopping, notify)
}

#[cfg(windows)]
fn attach_parent_console() -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER,
            INVALID_HANDLE_VALUE,
        },
        System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE, SetStdHandle,
        },
    };

    // A GUI-subsystem executable has no console by default. Attach before registering
    // ctrlc: AttachConsole resets console handlers and initializes missing std handles.
    // SAFETY: These process-owned handles remain valid across attachment; none are closed.
    unsafe {
        let inherited = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .map(|kind| (kind, GetStdHandle(kind)));
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error().map(|code| code as u32) {
                // Already attached, detached parent, or parent exited: no new console.
                Some(ERROR_ACCESS_DENIED | ERROR_INVALID_HANDLE | ERROR_INVALID_PARAMETER) => {
                    Ok(())
                }
                _ => Err(error),
            };
        }
        // Keep shell/service redirection instead of replacing it with console output.
        for (kind, handle) in inherited {
            if !handle.is_null()
                && handle != INVALID_HANDLE_VALUE
                && SetStdHandle(kind, handle) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

fn run_worker(
    config: Config,
    data: PathBuf,
    stopping: Arc<AtomicBool>,
    notify: Notify,
) -> Result<()> {
    if stopping.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut agent = build(&config, &data, Arc::clone(&notify))?;
    agent.stopping = stopping;
    if agent.stopping.load(Ordering::Relaxed) {
        return Ok(());
    }
    agent.api.connect()?;
    let serial = &agent
        .printer
        .last_status
        .as_ref()
        .expect("build probes printer")
        .unique_number;
    notify(
        &format!("Połączono: {serial}. Oczekiwanie na zlecenia."),
        "success",
    );
    agent.run_forever();
    Ok(())
}

struct Worker {
    stopping: Arc<AtomicBool>,
    messages: Receiver<(String, String)>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl Worker {
    fn spawn(config: Config, data: PathBuf, context: egui::Context) -> std::io::Result<Self> {
        let (sender, messages) = channel();
        let notify: Notify = Arc::new(move |message: &str, level: &str| {
            sender.send((message.to_string(), level.to_string())).ok();
            context.request_repaint();
        });
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = std::thread::Builder::new()
            .name("fiscal-agent".into())
            .spawn(move || run_worker(config, data, stop, notify))?;
        Ok(Self {
            stopping,
            messages,
            thread: Some(thread),
        })
    }

    fn finish(&mut self) -> Option<Result<()>> {
        if !self.thread.as_ref()?.is_finished() {
            return None;
        }
        Some(self.thread.take().unwrap().join().unwrap_or_else(|_| {
            Err(Error::value("Wątek drukarki zakończył się niespodziewanie. Sprawdź drukarkę i rejestr operacji."))
        }))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Even an OS/window shutdown must not detach an in-flight fiscal operation.
            let _ = thread.join();
        }
    }
}

struct App {
    data: PathBuf,
    config: Config,
    ports: Vec<String>,
    status: (String, egui::Color32),
    worker: Option<Worker>,
    tray: Option<tray::Tray>,
    /// Set by Zakończ: the window may close once the worker has finished its job.
    closing: bool,
}

const ERROR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x4d, 0x4f);
const SUCCESS: egui::Color32 = egui::Color32::from_rgb(0x52, 0xc4, 0x1a);
const PLAIN: egui::Color32 = egui::Color32::GRAY;

impl App {
    fn new(data: PathBuf, tray: Option<tray::Tray>) -> Self {
        Self {
            config: Config::load(&data.join("fiscal.json")),
            data,
            ports: posnet::candidate_ports(),
            status: ("Podaj adres API, token i port drukarki.".to_string(), PLAIN),
            worker: None,
            tray,
            closing: false,
        }
    }

    fn save(&mut self) -> bool {
        let checked = match self.config.validated() {
            Ok(config) => config,
            Err(error) => {
                self.status = (format!("Błąd: {error}"), ERROR);
                return false;
            }
        };
        match checked.save(&self.data.join("fiscal.json")) {
            Ok(()) => {
                self.config = checked;
                self.status = ("Zapisano ustawienia.".into(), SUCCESS);
                true
            }
            Err(error) => {
                self.status = (format!("Błąd: {error}"), ERROR);
                false
            }
        }
    }

    fn start(&mut self, context: &egui::Context) {
        if !self.save() {
            return;
        }
        if self.config.serial.is_empty() {
            self.status = ("Wybierz lub wpisz port drukarki.".into(), ERROR);
            return;
        }
        match Worker::spawn(self.config.clone(), self.data.clone(), context.clone()) {
            Ok(worker) => {
                self.status = ("Łączenie z drukarką i API…".into(), PLAIN);
                self.worker = Some(worker);
            }
            Err(error) => self.status = (format!("Błąd: {error}"), ERROR),
        }
    }

    /// A stop request is honoured only between jobs, never mid-receipt.
    fn stop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.stopping.store(true, Ordering::Relaxed);
            self.status = ("Zatrzymywanie po bieżącym zleceniu…".into(), PLAIN);
        }
    }

    /// Zakończ: stop the worker first, then let the window close for good.
    fn quit(&mut self, context: &egui::Context) {
        self.closing = true;
        if self.worker.is_some() {
            self.stop();
        } else {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn show(context: &egui::Context) {
        context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        context.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Closing the window only hides it: the agent must keep printing from the tray.
    fn handle_close(&mut self, context: &egui::Context) {
        if !context.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.closing && self.worker.is_none() {
            return;
        }
        context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.tray.is_some() {
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else {
            self.status = (
                "Zasobnik systemowy jest niedostępny. Aby zakończyć aplikację, kliknij Zakończ."
                    .into(),
                ERROR,
            );
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(worker) = &mut self.worker {
            while let Ok((message, level)) = worker.messages.try_recv() {
                let colour = match level.as_str() {
                    "error" => ERROR,
                    "success" => SUCCESS,
                    _ => PLAIN,
                };
                self.status = (message, colour);
            }
            if let Some(result) = worker.finish() {
                self.status = match result {
                    Ok(()) => ("Zatrzymano.".into(), PLAIN),
                    Err(error) => (format!("Błąd: {error}"), ERROR),
                };
                self.worker = None;
                if self.closing {
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            context.request_repaint_after(Duration::from_millis(250));
        }
        while let Some(command) = self
            .tray
            .as_ref()
            .and_then(|tray| tray.commands.try_recv().ok())
        {
            match command {
                tray::Command::Open => Self::show(context),
                tray::Command::Quit => self.quit(context),
            }
        }
        self.handle_close(context);
        let running = self.worker.is_some();

        egui::CentralPanel::default().show(context, |ui| {
            ui.heading("Drukarka fiskalna");
            ui.label("Agent drukuje paragony ze zleceń HuggingCar.");
            ui.add_space(12.0);

            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Adres API");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.api_url)
                                .hint_text("https://huggingcar.example/")
                                .desired_width(320.0),
                        );
                        ui.end_row();

                        ui.label("Token");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.token)
                                .password(true)
                                .desired_width(320.0),
                        );
                        ui.end_row();

                        ui.label("Port drukarki");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.config.serial)
                                    .hint_text("COM3 lub /dev/ttyACM0")
                                    .desired_width(160.0),
                            );
                            egui::ComboBox::from_id_salt("port")
                                .selected_text("Wybierz")
                                .width(60.0)
                                .show_ui(ui, |ui| {
                                    for port in &self.ports {
                                        ui.selectable_value(
                                            &mut self.config.serial,
                                            port.clone(),
                                            port,
                                        );
                                    }
                                });
                            if ui.button("Odśwież").clicked() {
                                self.ports = posnet::candidate_ports();
                            }
                        });
                        ui.end_row();

                        ui.label("Prędkość");
                        egui::ComboBox::from_id_salt("baudrate")
                            .selected_text(self.config.baudrate.to_string())
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for rate in BAUDRATES {
                                    ui.selectable_value(
                                        &mut self.config.baudrate,
                                        rate,
                                        rate.to_string(),
                                    );
                                }
                            });
                        ui.end_row();
                    });
            });

            ui.add_space(16.0);
            let mut quit = false;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!running, egui::Button::new("Zapisz"))
                    .clicked()
                {
                    self.save();
                }
                if ui
                    .add_enabled(!running, egui::Button::new("Uruchom"))
                    .clicked()
                {
                    self.start(context);
                }
                if ui
                    .add_enabled(
                        running
                            && !self
                                .worker
                                .as_ref()
                                .is_some_and(|worker| worker.stopping.load(Ordering::Relaxed)),
                        egui::Button::new("Zatrzymaj"),
                    )
                    .clicked()
                {
                    self.stop();
                }
                if ui.button("Zakończ").clicked() {
                    quit = true;
                }
            });
            if quit {
                self.quit(context);
            }

            ui.add_space(12.0);
            ui.colored_label(self.status.1, &self.status.0);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn wait_for_finish(worker: &mut Worker) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = worker.finish() {
                return result;
            }
            assert!(Instant::now() < deadline, "worker did not terminate");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn worker_startup_errors_and_panics_are_observable() {
        let data = tempfile::tempdir().unwrap();
        let mut worker = Worker::spawn(
            Config::default(),
            data.path().into(),
            egui::Context::default(),
        )
        .unwrap();
        assert!(wait_for_finish(&mut worker).is_err());
        let (_sender, messages) = channel();
        let mut panicked = Worker {
            stopping: Arc::new(AtomicBool::new(false)),
            messages,
            thread: Some(std::thread::spawn(|| panic!("worker panic"))),
        };
        assert!(wait_for_finish(&mut panicked).is_err());
        assert!(panicked.thread.is_none());
    }

    #[test]
    fn worker_drop_requests_stop_and_joins_without_detaching() {
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        let (_sender, messages) = channel();
        let worker = Worker {
            stopping,
            messages,
            thread: Some(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::yield_now();
                }
                done.store(true, Ordering::Relaxed);
                Ok(())
            })),
        };
        drop(worker);
        assert!(finished.load(Ordering::Relaxed));
    }

    #[test]
    fn quit_keeps_worker_owned_until_completion() {
        let data = tempfile::tempdir().unwrap();
        let mut app = App::new(data.path().into(), None);
        let stopping = Arc::new(AtomicBool::new(false));
        let (_sender, messages) = channel();
        let (release, wait) = channel();
        app.worker = Some(Worker {
            stopping: Arc::clone(&stopping),
            messages,
            thread: Some(std::thread::spawn(move || {
                wait.recv().unwrap();
                Ok(())
            })),
        });
        app.quit(&egui::Context::default());
        assert!(app.closing);
        assert!(stopping.load(Ordering::Relaxed));
        assert!(app.worker.as_mut().unwrap().finish().is_none());
        release.send(()).unwrap();
        wait_for_finish(app.worker.as_mut().unwrap()).unwrap();
    }

    #[test]
    fn cli_supports_isolated_state_and_rejects_missing_path() {
        let args = ["--data-dir", "/tmp/agent-custom", "--headless"].map(Into::into);
        assert_eq!(
            options(args.into_iter()).unwrap(),
            (PathBuf::from("/tmp/agent-custom"), true)
        );
        assert!(options(["--data-dir"].map(Into::into).into_iter()).is_err());
        assert!(options(["--unknown"].map(Into::into).into_iter()).is_err());
    }
}
