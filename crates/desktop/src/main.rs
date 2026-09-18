#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ui;

use std::path::PathBuf;

use fiscal_desktop::{Controller, Settings, data_dir, status_json};
use posnet::Printer;

struct Args {
    probe: bool,
    serial: Option<String>,
    baudrate: Option<u32>,
    data: PathBuf,
}
fn arguments() -> Result<Option<Args>, String> {
    let mut args = Args {
        probe: false,
        serial: None,
        baudrate: None,
        data: PathBuf::new(),
    };
    let mut use_default_data = true;
    let mut input = std::env::args().skip(1);
    while let Some(argument) = input.next() {
        let (key, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(key, value)| {
                (key, Some(value.to_owned()))
            });
        match key {
            "--help" | "-h" => {
                println!(
                    "HuggingCar Fiscal · Posnet Temo Online\n\nOpcje:\n  --probe            Odczytaj status i VAT (JSON), bez okna\n  --serial PORT      Port szeregowy drukarki\n  --baudrate SPEED   Prędkość transmisji (domyślnie 9600)\n  --data-dir PATH    Katalog rejestru i historii\n  --help             Pokaż pomoc"
                );
                return Ok(None);
            }
            "--probe" if inline.is_none() => args.probe = true,
            "--serial" | "--baudrate" | "--data-dir" => {
                let value = inline
                    .or_else(|| input.next())
                    .ok_or_else(|| format!("Brak wartości opcji {key}"))?;
                match key {
                    "--serial" => args.serial = Some(value),
                    "--baudrate" => {
                        args.baudrate = Some(
                            value
                                .parse::<u32>()
                                .ok()
                                .filter(|n| *n > 0)
                                .ok_or("Nieprawidłowa prędkość transmisji")?,
                        )
                    }
                    _ => {
                        args.data = PathBuf::from(value);
                        use_default_data = false;
                    }
                }
            }
            _ => return Err(format!("Nieznana opcja: {argument}")),
        }
    }
    if use_default_data {
        args.data = data_dir()?;
    }
    Ok(Some(args))
}

fn main() {
    let args = match arguments() {
        Ok(Some(args)) => args,
        Ok(None) => return,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let mut settings = match Settings::load(&args.data) {
        Ok(settings) => settings,
        Err(error) => {
            failure(error, args.probe);
            return;
        }
    };
    // A new serial port starts at 9600 unless --baudrate is given.
    if let Some(serial) = args.serial {
        settings.address = serial;
        settings.baudrate = 9600;
    }
    if let Some(baudrate) = args.baudrate {
        settings.baudrate = baudrate;
    }
    if args.probe {
        match Printer::new(settings.connection(), args.data.join("operation.json"))
            .and_then(|mut printer| printer.probe())
        {
            Ok(status) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status_json(&status)).expect("JSON status")
                );
                if !status.ready {
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let controller = match Controller::new(settings, args.data) {
        Ok(controller) => controller,
        Err(error) => {
            failure(error, false);
            return;
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([1040.0, 780.0])
            .with_app_id("huggingcar-fiscal")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("embedded fiscal icon"),
            ),
        ..Default::default()
    };
    if let Err(error) = eframe::run_native(
        "HuggingCar Fiscal",
        options,
        Box::new(move |context| Ok(Box::new(ui::App::new(context, controller)))),
    ) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn failure(message: String, probe: bool) {
    eprintln!("{message}");
    if !probe {
        struct Failure(String);
        impl eframe::App for Failure {
            fn update(&mut self, context: &eframe::egui::Context, _: &mut eframe::Frame) {
                eframe::egui::CentralPanel::default().show(context, |ui| {
                    ui.heading("Nie można uruchomić aplikacji");
                    ui.label(&self.0);
                    if ui.button("Zamknij").clicked() {
                        context.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                    }
                });
            }
        }
        let options = eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default().with_inner_size([520.0, 200.0]),
            ..Default::default()
        };
        let _ = eframe::run_native(
            "HuggingCar Fiscal",
            options,
            Box::new(move |_| Ok(Box::new(Failure(message)))),
        );
    }
    std::process::exit(1);
}
