use std::time::Duration;

use chrono::{Datelike, Days, Local, Months, NaiveDate};
use eframe::egui::{self, Color32, RichText};
use fiscal_desktop::{Controller, Job, Settings, completed_months};
use posnet::{money, validate_name};
use serde_json::Value;

const BLUE: Color32 = Color32::from_rgb(22, 119, 255);
const RED: Color32 = Color32::from_rgb(210, 45, 50);
const MONTHS: [&str; 12] = [
    "styczeń",
    "luty",
    "marzec",
    "kwiecień",
    "maj",
    "czerwiec",
    "lipiec",
    "sierpień",
    "wrzesień",
    "październik",
    "listopad",
    "grudzień",
];
fn month_label(year: i32, month: u32) -> String {
    format!("{} {year}", MONTHS[month as usize - 1])
}
fn payment_label(payment: i64) -> &'static str {
    if payment == 2 {
        "Karta płatnicza"
    } else {
        "Gotówka"
    }
}

struct Confirmation {
    title: String,
    text: String,
    action: String,
    job: Job,
}
struct SettingsEditor {
    draft: Settings,
    selected: Vec<bool>,
    tab: usize,
    ports: Vec<String>,
    error: String,
}
struct Calendar {
    end: bool,
    month: NaiveDate,
}

pub struct App {
    model: Controller,
    tab: usize,
    confirmation: Option<Confirmation>,
    settings: Option<SettingsEditor>,
    months: Vec<(i32, u32)>,
    month: usize,
    period_start: String,
    period_end: String,
    summary: bool,
    calendar: Option<Calendar>,
    history: Vec<Value>,
    history_error: String,
}

impl App {
    pub fn new(context: &eframe::CreationContext<'_>, mut model: Controller) -> Self {
        egui_extras::install_image_loaders(&context.egui_ctx);
        context.egui_ctx.set_visuals(egui::Visuals::light());
        let mut style = (*context.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(12.0, 10.0);
        style.spacing.button_padding = egui::vec2(14.0, 9.0);
        style.visuals.selection.bg_fill = BLUE;
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
        context.egui_ctx.set_style(style);
        let today = Local::now().date_naive();
        model.start(Job::Probe { scan: true });
        Self {
            model,
            tab: 0,
            confirmation: None,
            settings: None,
            months: completed_months(today),
            month: 0,
            period_start: (today - Days::new(7)).format("%d.%m.%Y").to_string(),
            period_end: today.format("%d.%m.%Y").to_string(),
            summary: false,
            calendar: None,
            history: vec![],
            history_error: String::new(),
        }
    }
    fn confirm(&mut self, title: &str, text: String, action: &str, job: Job) {
        if self.model.busy() {
            return;
        }
        self.confirmation = Some(Confirmation {
            title: title.into(),
            text,
            action: action.into(),
            job,
        });
    }
    fn refresh_history(&mut self) {
        match self.model.history() {
            Ok(rows) => {
                self.history = rows;
                self.history_error.clear();
            }
            Err(error) => {
                self.history.clear();
                self.history_error = format!("Nie można odczytać historii: {error}");
            }
        }
    }
    fn sales(&mut self, ui: &mut egui::Ui) {
        let editable = self.model.editable();
        let ready = self.model.ready();
        let vat = self.model.vat.unwrap_or(0);
        // Compute over every row, not the filtered view. Unpriced rows still validate names.
        let (mut total, mut count, mut validation) = (0, 0, None);
        for (index, row) in self.model.rows.iter().enumerate() {
            match row.line(vat) {
                Ok(Some(line)) => {
                    total += line.total_cents();
                    count += 1;
                }
                Err(error) if validation.is_none() => {
                    validation = Some(format!("Wiersz {}: {error}", index + 1))
                }
                _ => {}
            }
        }
        if count > 500 || total > posnet::models::MAX_CENTS {
            validation = Some("Paragon przekracza zakres drukarki.".into());
        }
        let width = ui.available_width();
        ui.horizontal_top(|ui| {
            egui::Frame::group(ui.style()).inner_margin(20).show(ui, |ui| {
                ui.vertical(|ui| {
                ui.set_width((width - 385.0).max(530.0));
                ui.horizontal(|ui| {
                    ui.heading("Pozycje paragonu");
                    if ui.add_enabled(editable, egui::Button::new("Wyczyść ceny")).clicked() { self.model.clear_prices(); }
                });
                ui.add(egui::TextEdit::singleline(&mut self.model.search).hint_text("Szukaj usługi").desired_width(f32::INFINITY));
                if let Some(error) = &validation { ui.colored_label(RED, error); }
                else { ui.weak("Do paragonu trafią tylko usługi z ceną większą od zera."); }
                ui.separator();
                let needle = self.model.search.trim().to_lowercase();
                let name_width = (ui.available_width() - 280.0).max(200.0);
                ui.add_enabled_ui(editable, |ui| {
                    egui::ScrollArea::vertical().id_salt("sale_scroll").max_height(ui.available_height() - 25.0).show(ui, |ui| {
                        egui::Grid::new("sale_rows").num_columns(4).striped(true).spacing([10.0, 10.0]).show(ui, |ui| {
                            ui.strong("Nazwa usługi"); ui.strong("Ilość"); ui.strong("Cena brutto"); ui.strong("Wartość"); ui.end_row();
                            for (index, row) in self.model.rows.iter_mut().enumerate() {
                                if !row.name.to_lowercase().contains(&needle) { continue; }
                                    if let Some(error) = name_editor(ui, &mut row.name, name_width, egui::Id::new(("sale_name", index))) { self.model.message = error; }
                                    ui.add_sized([65.0, 24.0], egui::TextEdit::singleline(&mut row.quantity).id(egui::Id::new(("sale_quantity", index))));
                                    ui.add_sized([85.0, 24.0], egui::TextEdit::singleline(&mut row.price).id(egui::Id::new(("sale_price", index))).hint_text("0,00"));
                                    match row.line(vat) {
                                        Ok(Some(line)) => { ui.label(money(line.total_cents())); }
                                        Ok(None) => { ui.weak("0,00 zł"); }
                                        Err(_) => { ui.colored_label(RED, "Sprawdź"); }
                                    }
                                    ui.end_row();
                            }
                        });
                    });
                });
                });
            });
            egui::Frame::group(ui.style()).inner_margin(20).show(ui, |ui| {
                ui.vertical(|ui| {
                ui.set_width(280.0);
                ui.heading("Podsumowanie"); ui.add_space(10.0);
                ui.label("Do zapłaty brutto");
                ui.label(RichText::new(money(total)).size(34.0).strong());
                if let Some(rate) = self.model.vat_rates.iter().find(|rate| Some(rate.index) == self.model.vat) {
                    let tax = rate.tax_cents(total);
                    ui.label(format!("Netto {}", money(total - tax)));
                    ui.label(format!("VAT {} {}", rate.rate_label(), money(tax)));
                }
                ui.weak(format!("Pozycji na paragonie: {count}"));
                ui.separator();
                ui.add_enabled_ui(editable, |ui| {
                    ui.label("Stawka VAT");
                    let rates = &self.model.vat_rates;
                    if !rates.is_empty() {
                        let selected = rates.iter().find(|rate| Some(rate.index) == self.model.vat).map(|rate| rate.label()).unwrap_or_else(|| "Wybierz stawkę VAT".into());
                        egui::ComboBox::from_id_salt("vat").selected_text(selected).width(250.0).show_ui(ui, |ui| {
                            for rate in rates { ui.selectable_value(&mut self.model.vat, Some(rate.index), rate.label()); }
                        });
                    } else { ui.weak("Oczekiwanie na drukarkę"); }
                    ui.add_space(10.0); ui.label("Forma płatności");
                    egui::ComboBox::from_id_salt("payment").selected_text(payment_label(self.model.payment)).width(250.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.model.payment, 0, "Gotówka");
                        ui.selectable_value(&mut self.model.payment, 2, "Karta płatnicza");
                    });
                });
                ui.add_space(30.0);
                let valid = validation.is_none() && count > 0 && self.model.vat.is_some();
                if ui.add_enabled(ready && valid, egui::Button::new(RichText::new("Drukuj paragon").color(Color32::WHITE)).fill(BLUE).min_size(egui::vec2(250.0, 44.0))).clicked() {
                    // Re-read after this frame's edits, so confirmation and payload agree.
                    match self.model.receipt_lines() {
                        Ok(lines) if !lines.is_empty() => {
                            let total = lines.iter().map(posnet::Line::total_cents).sum();
                            let text = format!("Pozycji: {}\nDo zapłaty: {}\nPłatność: {}\n\nZatwierdzenie wystawi paragon fiskalny. Sprawdź dane przed drukowaniem.", lines.len(), money(total), payment_label(self.model.payment));
                            self.confirm("Potwierdź paragon", text, "Drukuj", Job::Receipt(lines, self.model.payment));
                        }
                        Err(error) => self.model.message = error,
                        _ => {}
                    }
                }
                });
            });
        });
    }
    fn reports(&mut self, ui: &mut egui::Ui) {
        let ready = self.model.ready();
        let editable = self.model.editable();
        egui::Frame::group(ui.style()).inner_margin(24).show(ui, |ui| {
            ui.heading("Raporty fiskalne"); ui.add_space(20.0);
            ui.strong("Raport dobowy");
            ui.label("Zamyka sprzedaż bieżącego dnia w pamięci fiskalnej. Wykonuj po zakończeniu sprzedaży.");
            if ui.add_enabled(ready, egui::Button::new("Drukuj raport dobowy")).clicked() {
                self.confirm("Raport dobowy", "Raport dobowy zamknie bieżącą sprzedaż dnia w pamięci fiskalnej. Wydrukować raport?".into(), "Drukuj", Job::Daily);
            }
            ui.add_space(15.0); ui.separator(); ui.add_space(15.0);
            ui.strong("Raport miesięczny"); ui.label("Łączny raport fiskalny za wybrany zakończony miesiąc.");
            ui.horizontal(|ui| {
                ui.add_enabled_ui(editable, |ui| {
                    let (year, month) = self.months[self.month];
                    egui::ComboBox::from_id_salt("report_month").selected_text(month_label(year, month)).width(220.0).show_ui(ui, |ui| {
                        for (index, &(year, month)) in self.months.iter().enumerate() { ui.selectable_value(&mut self.month, index, month_label(year, month)); }
                    });
                });
                if ui.add_enabled(ready, egui::Button::new("Drukuj raport miesięczny")).clicked() {
                    let (year, month) = self.months[self.month];
                    self.confirm("Raport miesięczny", format!("Wydrukować raport miesięczny za {}?", month_label(year, month)), "Drukuj", Job::Monthly(year, month));
                }
            });
            ui.add_space(15.0); ui.separator(); ui.add_space(15.0);
            ui.strong("Raport okresowy"); ui.label("Za dowolny zakres dat: szczegółowy lub skrócony (łączny).");
            ui.add_enabled_ui(editable, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Od"); ui.add(egui::TextEdit::singleline(&mut self.period_start).desired_width(110.0).hint_text("dd.mm.rrrr"));
                    if ui.button("Kalendarz od").clicked() { self.open_calendar(false); }
                    ui.label("Do"); ui.add(egui::TextEdit::singleline(&mut self.period_end).desired_width(110.0).hint_text("dd.mm.rrrr"));
                    if ui.button("Kalendarz do").clicked() { self.open_calendar(true); }
                    ui.checkbox(&mut self.summary, "Skrócony");
                });
            });
            if ui.add_enabled(ready, egui::Button::new("Drukuj raport okresowy")).clicked() {
                let dates = NaiveDate::parse_from_str(&self.period_start, "%d.%m.%Y").and_then(|start| NaiveDate::parse_from_str(&self.period_end, "%d.%m.%Y").map(|end| (start, end)));
                match dates {
                    Ok((start, end)) if start > end => self.model.message = "Data początkowa nie może być późniejsza niż końcowa.".into(),
                    Ok((_, end)) if end > Local::now().date_naive() => self.model.message = "Data raportu nie może być późniejsza niż dzisiejsza.".into(),
                    Ok((start, end)) => {
                        let kind = if self.summary { "skrócony" } else { "szczegółowy" };
                        self.confirm("Raport okresowy", format!("Wydrukować raport {kind} za okres {} – {}?", start.format("%d.%m.%Y"), end.format("%d.%m.%Y")), "Drukuj", Job::Periodic(start, end, self.summary));
                    }
                    Err(_) => self.model.message = "Podaj poprawne daty w formacie dd.mm.rrrr.".into(),
                }
            }
        });
    }
    fn history(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Wydrukowane paragony");
            if ui.button("Odśwież historię").clicked() {
                self.refresh_history();
            }
        });
        ui.weak("Lista z tego komputera. Numer nadaje drukarka. Ostatnie 500 paragonów.");
        if !self.history_error.is_empty() {
            ui.colored_label(RED, &self.history_error);
        } else if self.history.is_empty() {
            ui.label("Brak paragonów wydrukowanych z tego komputera.");
        }
        egui::ScrollArea::both().show(ui, |ui| {
            egui::Grid::new("history_rows")
                .num_columns(5)
                .striped(true)
                .spacing([20.0, 12.0])
                .show(ui, |ui| {
                    for header in ["Pozycje", "Data", "Nr paragonu", "Kwota", "Płatność"] {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for record in &self.history {
                        let names = record
                            .get("lines")
                            .and_then(Value::as_array)
                            .map(|lines| {
                                lines
                                    .iter()
                                    .filter_map(|line| line.get("name").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        ui.add(egui::Label::new(names).wrap().selectable(true));
                        let date: String = record
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(16)
                            .collect();
                        ui.label(date.replace('T', " "));
                        ui.label(
                            record
                                .get("receipt_number")
                                .and_then(Value::as_str)
                                .filter(|n| !n.is_empty())
                                .unwrap_or("—"),
                        );
                        ui.label(money(
                            record
                                .get("total_cents")
                                .and_then(Value::as_i64)
                                .unwrap_or(0),
                        ));
                        ui.label(match record.get("payment").and_then(Value::as_i64) {
                            Some(0) => "Gotówka",
                            Some(2) => "Karta płatnicza",
                            _ => "—",
                        });
                        ui.end_row();
                    }
                });
        });
    }
    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.settings else {
            return;
        };
        let mut close = false;
        let mut save = false;
        egui::Modal::new(egui::Id::new("settings_modal")).show(ctx, |ui| {
            ui.set_width(650.0); ui.heading("Ustawienia");
            ui.horizontal(|ui| { ui.selectable_value(&mut editor.tab, 0, "Usługi"); ui.selectable_value(&mut editor.tab, 1, "Drukarka"); });
            ui.separator();
            if editor.tab == 0 {
                ui.weak("Edytuj nazwy. Zaznacz usługi do usunięcia. Przyciski Góra / Dół zmieniają kolejność.");
                ui.horizontal(|ui| {
                    if ui.button("Dodaj").clicked() { editor.draft.services.push("Nowa usługa".into()); editor.selected.push(false); }
                    if ui.button("Usuń zaznaczone").clicked() {
                        let mut index = 0;
                        editor.draft.services.retain(|_| { let keep = !editor.selected[index]; index += 1; keep });
                        editor.selected = vec![false; editor.draft.services.len()];
                    }
                    if ui.button("Przywróć domyślne usługi").clicked() { editor.draft.services = Settings::default().services; editor.selected = vec![false; editor.draft.services.len()]; }
                });
                let mut reorder = None;
                let length = editor.draft.services.len();
                egui::ScrollArea::vertical().id_salt("settings_services").max_height(400.0).show(ui, |ui| {
                    for (index, name) in editor.draft.services.iter_mut().enumerate() {
                        ui.push_id(index, |ui| { ui.horizontal(|ui| {
                            ui.checkbox(&mut editor.selected[index], "");
                            if let Some(error) = name_editor(ui, name, 365.0, egui::Id::new(("catalog_name", index))) { editor.error = error; }
                            if ui.add_enabled(index > 0, egui::Button::new("Góra")).clicked() { reorder = Some((index, index - 1)); }
                            if ui.add_enabled(index + 1 < length, egui::Button::new("Dół")).clicked() { reorder = Some((index, index + 1)); }
                        }); });
                    }
                });
                if let Some((from, to)) = reorder { editor.draft.services.swap(from, to); editor.selected.swap(from, to); }
            } else {
                ui.label("Port drukarki (pusty: wykrywanie automatyczne)");
                ui.text_edit_singleline(&mut editor.draft.address);
                egui::ComboBox::from_id_salt("available_ports").selected_text("Wybierz dostępny port").show_ui(ui, |ui| {
                    for port in &editor.ports { ui.selectable_value(&mut editor.draft.address, port.clone(), port); }
                });
                ui.label("Prędkość transmisji");
                egui::ComboBox::from_id_salt("baudrate").selected_text(editor.draft.baudrate.to_string()).show_ui(ui, |ui| {
                    for baud in [9600, 19200, 38400, 57600, 115200] { ui.selectable_value(&mut editor.draft.baudrate, baud, baud.to_string()); }
                });
                ui.weak("Drukarka jest wykrywana automatycznie. Wskaż port ręcznie tylko, gdy wykrywanie zawodzi.");
            }
            if !editor.error.is_empty() { ui.colored_label(RED, &editor.error); }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Anuluj").clicked() { close = true; }
                if ui.button("Zapisz").clicked() { save = true; }
            });
        });
        if save {
            match self.model.save_settings(editor.draft.clone()) {
                Ok(()) => close = true,
                Err(error) => {
                    editor.error = error;
                    editor.tab = 0;
                }
            }
        }
        if close {
            self.settings = None;
        }
    }
    fn confirmation_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirmation else {
            return;
        };
        let mut accepted = false;
        let mut cancelled = false;
        egui::Modal::new(egui::Id::new("fiscal_confirmation")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(&confirm.title);
            ui.add_space(10.0);
            ui.label(&confirm.text);
            if let Some(status) = &self.model.status {
                ui.weak(format!("Drukarka: {}", status.unique_number));
            }
            ui.add_space(15.0);
            ui.horizontal(|ui| {
                if ui.button("Anuluj").clicked() {
                    cancelled = true;
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new(&confirm.action).color(Color32::WHITE))
                            .fill(BLUE),
                    )
                    .clicked()
                {
                    accepted = true;
                }
            });
        });
        if accepted {
            if let Some(confirmation) = self.confirmation.take() {
                self.model.start(confirmation.job);
            }
        } else if cancelled {
            self.confirmation = None;
        }
    }
    fn open_calendar(&mut self, end: bool) {
        let date = NaiveDate::parse_from_str(
            if end {
                &self.period_end
            } else {
                &self.period_start
            },
            "%d.%m.%Y",
        )
        .unwrap_or_else(|_| Local::now().date_naive());
        self.calendar = Some(Calendar {
            end,
            month: date.with_day(1).expect("first day"),
        });
    }
    fn calendar_dialog(&mut self, ctx: &egui::Context) {
        let Some(calendar) = &mut self.calendar else {
            return;
        };
        let today = Local::now().date_naive();
        let mut selected = None;
        let mut close = false;
        egui::Modal::new(egui::Id::new("calendar")).show(ctx, |ui| {
            ui.heading(if calendar.end {
                "Data końcowa"
            } else {
                "Data początkowa"
            });
            ui.horizontal(|ui| {
                if ui.button("Poprzedni").clicked()
                    && let Some(month) = calendar.month.checked_sub_months(Months::new(1))
                {
                    calendar.month = month;
                }
                ui.strong(month_label(calendar.month.year(), calendar.month.month()));
                if ui
                    .add_enabled(
                        calendar.month.year() < today.year()
                            || calendar.month.month() < today.month(),
                        egui::Button::new("Następny"),
                    )
                    .clicked()
                    && let Some(month) = calendar.month.checked_add_months(Months::new(1))
                {
                    calendar.month = month;
                }
            });
            let first = calendar.month.weekday().num_days_from_monday();
            egui::Grid::new("calendar_days")
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    for day in ["Pon", "Wt", "Śr", "Czw", "Pt", "Sob", "Ndz"] {
                        ui.strong(day);
                    }
                    ui.end_row();
                    for cell in 0..42 {
                        let date = (cell >= first)
                            .then(|| {
                                calendar
                                    .month
                                    .checked_add_days(Days::new((cell - first) as u64))
                            })
                            .flatten()
                            .filter(|date| date.month() == calendar.month.month());
                        if let Some(date) = date {
                            if ui
                                .add_enabled(
                                    date <= today,
                                    egui::Button::new(date.day().to_string())
                                        .min_size(egui::vec2(36.0, 30.0)),
                                )
                                .clicked()
                            {
                                selected = Some(date);
                            }
                        } else {
                            ui.label("");
                        }
                        if cell % 7 == 6 {
                            ui.end_row();
                        }
                    }
                });
            if ui.button("Anuluj").clicked() {
                close = true;
            }
        });
        if let Some(date) = selected {
            let field = if calendar.end {
                &mut self.period_end
            } else {
                &mut self.period_start
            };
            *field = date.format("%d.%m.%Y").to_string();
            close = true;
        }
        if close {
            self.calendar = None;
        }
    }
}

fn name_editor(
    ui: &mut egui::Ui,
    name: &mut String,
    width: f32,
    widget_id: egui::Id,
) -> Option<String> {
    let before = name.clone();
    let response = ui.add_sized(
        [width, 24.0],
        egui::TextEdit::singleline(name)
            .id(widget_id)
            .desired_width(width),
    );
    let id = response.id.with("previous_name");
    if response.gained_focus() {
        ui.data_mut(|data| data.insert_temp(id, before));
    }
    if response.lost_focus() {
        match validate_name(name) {
            Ok(valid) => *name = valid,
            Err(error) => {
                if let Some(previous) = ui.data(|data| data.get_temp::<String>(id)) {
                    *name = previous;
                }
                return Some(format!("Nie zmieniono nazwy. {error}"));
            }
        }
    }
    None
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.model.poll();
        if self.model.retry_due()
            && self.confirmation.is_none()
            && self.settings.is_none()
            && self.calendar.is_none()
        {
            self.model.start(Job::Probe {
                scan: self.model.settings.address.is_empty(),
            });
        }
        if self.model.offer_force && self.confirmation.is_none() {
            self.model.offer_force = false;
            self.confirm("Drukarka niedostępna", format!("{}\n\nJeśli drukarka jest uszkodzona lub wymieniona, blokadę można usunąć bez sprawdzenia. Zrób to tylko po ustaleniu na papierze, czy operacja się wykonała. Nie ponawiaj wydruku.", self.model.message), "Usuń blokadę mimo to", Job::Recover { force: true });
        }
        if ctx.input(|input| input.viewport().close_requested()) && self.model.busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.model.message =
                "Poczekaj na zakończenie operacji przed zamknięciem aplikacji.".into();
        }
        ctx.request_repaint_after(Duration::from_millis(if self.model.busy() {
            50
        } else {
            500
        }));
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(egui::include_image!("../assets/icon.svg"))
                        .fit_to_exact_size(egui::vec2(36.0, 36.0)),
                );
                ui.heading("HuggingCar Fiscal");
                ui.add_space(20.0);
                for (index, name) in ["Sprzedaż", "Raporty", "Historia"].iter().enumerate() {
                    if ui.selectable_value(&mut self.tab, index, *name).clicked() && index == 2 {
                        self.refresh_history();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.model.busy(), egui::Button::new("Ustawienia"))
                        .clicked()
                    {
                        let mut ports = posnet::candidate_ports();
                        ports.sort_by_key(|port| !port.contains("ttyACM"));
                        self.settings = Some(SettingsEditor {
                            selected: vec![false; self.model.settings.services.len()],
                            draft: self.model.settings.clone(),
                            tab: 0,
                            ports,
                            error: String::new(),
                        });
                    }
                });
            });
            ui.add_space(8.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if self.model.busy() { ui.spinner(); }
                ui.label(&self.model.message);
                if ui.add_enabled(!self.model.busy(), egui::Button::new("Sprawdź drukarkę")).clicked() { self.model.start(Job::Probe { scan: true }); }
                if self.model.pending.is_some() && ui.add_enabled(!self.model.busy(), egui::Button::new("Wyjaśnij ostatnią operację")).clicked() {
                    self.confirm("Potwierdź sprawdzenie drukarki", "Sprawdź fizyczny wydruk i stan transakcji na drukarce. Operacja mogła się zakończyć mimo braku odpowiedzi. Nie powtarzaj jej bez sprawdzenia.\n\nCzy ustalono wynik operacji i można usunąć lokalną blokadę? To potwierdzenie nie anuluje transakcji ani nie ponawia wydruku.".into(), "Usuń blokadę", Job::Recover { force: false });
                }
            }); ui.add_space(8.0);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(15.0);
            match self.tab {
                0 => self.sales(ui),
                1 => self.reports(ui),
                _ => self.history(ui),
            }
        });
        self.settings_dialog(ctx);
        self.calendar_dialog(ctx);
        self.confirmation_dialog(ctx);
    }
}
