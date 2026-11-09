//! COSMIC panel applet for the HP charge limiter daemon.
//!
//! The applet only talks to the root daemon over `/run/hp-charge-limit.sock`;
//! it never touches ACPI itself. Battery power figures are read unprivileged
//! from sysfs, purely to show an estimated time to the charge limit.

use std::collections::VecDeque;
use std::fs;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use cosmic::app::{Core, Task};
use cosmic::iced::core::window;
use cosmic::iced::gradient;
use cosmic::iced::mouse;
use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::time;
use cosmic::iced::window::Id;
use cosmic::iced::{
    alignment, Alignment, Background, Border, Color, Gradient, Length, Limits, Point, Rectangle,
    Shadow, Size, Subscription, Vector,
};
use cosmic::widget::{
    autosize::autosize, button, canvas, column, container, row, slider, space, text, toaster,
    toggler, tooltip, Toast, ToastId, Toasts,
};
use cosmic::{Element, Theme};

use crate::socket::{self, Status};

const ID: &str = "io.github.AceMythos.HpCosmicChargeLimit";
const BATTERY: &str = "/sys/class/power_supply/BAT0";
const SERVICE_COMMAND: &str = "sudo systemctl start hp-charge-limit";
const HISTORY_CAPACITY: usize = 600;
const POLL_SECONDS: u64 = 2;

/// Width of the popup in logical pixels. Fixed so the layout never reflows.
const POPUP_WIDTH: f32 = 360.0;
/// Corner radius shared by the popup surface and every card inside it.
const RADIUS: f32 = 12.0;

static POPUP_AUTOSIZE_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("hp-charge-limit-popup"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drag {
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Charging,
    Holding,
    OnBattery,
    Off,
    Unknown,
}

pub struct App {
    core: Core,
    popup: Option<Id>,
    status: Option<Status>,
    error: Option<String>,
    /// True while a slider or band handle is being dragged, so polls cannot move it.
    editing: bool,
    /// True while a write request is in flight.
    saving: bool,
    show_advanced: bool,
    /// Smoothed charge level used by the band marker.
    capacity_view: f32,
    estimate: Option<u32>,
    history: VecDeque<u8>,
    toasts: Toasts<Message>,
    last_update: Option<Instant>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            core: Core::default(),
            popup: None,
            status: None,
            error: None,
            editing: false,
            saving: false,
            show_advanced: false,
            capacity_view: 0.0,
            estimate: None,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            toasts: Toasts::new(Message::CloseToast),
            last_update: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    Refresh,
    Loaded(Result<Status, String>),
    Saved(Result<Status, String>),
    ToggleLimit(bool),
    StopChanged(f64),
    StartChanged(f64),
    Persist,
    ApplyPreset(u32, u32),
    BandDragged(Drag, u32),
    CommitBand,
    ToggleAdvanced,
    CopyCommand,
    Frame,
    CloseToast(ToastId),
}

impl cosmic::Application for App {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Message>) {
        let app = App {
            core,
            ..Default::default()
        };
        (app, fetch())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TogglePopup => {
                return if let Some(popup_id) = self.popup.take() {
                    destroy_popup(popup_id)
                } else {
                    let new_id = Id::unique();
                    self.popup = Some(new_id);

                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(400.0)
                        .min_width(360.0)
                        .min_height(420.0)
                        .max_height(800.0);

                    Task::batch([get_popup(popup_settings), fetch()])
                };
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                    // A drag that ended outside the popup must still be saved.
                    if self.editing {
                        self.editing = false;
                        if let Some(status) = self.status.clone() {
                            self.saving = true;
                            return send(set_request(status.start, status.stop));
                        }
                    }
                }
            }
            Message::Refresh => return fetch(),
            Message::Loaded(result) => match result {
                Ok(status) => self.accept_status(status, true),
                Err(error) => {
                    // A single failed poll should not blank a working popup.
                    self.error = Some(error);
                }
            },
            Message::Saved(result) => {
                self.saving = false;
                self.editing = false;
                match result {
                    Ok(status) => self.accept_status(status, false),
                    Err(error) => {
                        let short = short_error(&error);
                        self.error = Some(error);
                        return self
                            .toasts
                            .push(Toast::new(short).duration(Duration::from_secs(4)))
                            .map(cosmic::Action::App);
                    }
                }
            }
            Message::ToggleLimit(enabled) => {
                if let Some(status) = &mut self.status {
                    status.enabled = enabled;
                }
                self.saving = true;
                return send(format!(r#"{{"cmd":"enable","value":{enabled}}}"#));
            }
            Message::StopChanged(value) => {
                if let Some(status) = &mut self.status {
                    let lowest = (status.start + 1).min(100);
                    status.stop = (value.round() as u32).min(100).max(lowest);
                    self.editing = true;
                }
            }
            Message::StartChanged(value) => {
                if let Some(status) = &mut self.status {
                    status.start = (value.round() as u32)
                        .min(status.stop.saturating_sub(1));
                    self.editing = true;
                }
            }
            Message::Persist => {
                self.editing = false;
                if let Some(status) = &self.status {
                    self.saving = true;
                    return send(set_request(status.start, status.stop));
                }
            }
            Message::ApplyPreset(start, stop) => {
                if let Some(status) = &mut self.status {
                    status.start = start;
                    status.stop = stop;
                }
                self.editing = false;
                self.saving = true;
                return send(set_request(start, stop));
            }
            Message::BandDragged(handle, value) => {
                if let Some(status) = &mut self.status {
                    match handle {
                        Drag::Start => {
                            status.start = value.min(status.stop.saturating_sub(1));
                        }
                        Drag::Stop => {
                            status.stop = value.max(status.start + 1).min(100);
                        }
                    }
                    self.editing = true;
                }
            }
            Message::CommitBand => {
                self.editing = false;
                if let Some(status) = &self.status {
                    self.saving = true;
                    return send(set_request(status.start, status.stop));
                }
            }
            Message::ToggleAdvanced => {
                self.show_advanced = !self.show_advanced;
            }
            Message::CopyCommand => {
                return cosmic::iced::clipboard::write(SERVICE_COMMAND.to_string());
            }
            Message::Frame => {
                if let Some(status) = &self.status {
                    let target = status.capacity.unwrap_or(0) as f32;
                    let difference = target - self.capacity_view;
                    if difference.abs() < 0.25 {
                        self.capacity_view = target;
                    } else {
                        self.capacity_view += difference * 0.22;
                    }
                }
            }
            Message::CloseToast(id) => self.toasts.remove(id),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let phase = self.phase();
        let capacity = self.status.as_ref().and_then(|status| status.capacity);
        let label = capacity
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "—".to_string());

        let glyph = canvas::Canvas::<PanelGlyph, Message, Theme>::new(PanelGlyph {
            capacity: capacity.map(|value| value as f32),
            stop: self
                .status
                .as_ref()
                .map_or(100.0, |status| status.stop as f32),
            phase,
        })
        .width(Length::Fixed(26.0))
        .height(Length::Fixed(16.0));

        let content = row![glyph, text::body(label)]
            .spacing(6)
            .align_y(Alignment::Center);

        let button = button::custom(content)
            .class(cosmic::theme::Button::AppletIcon)
            .on_press_down(Message::TogglePopup)
            .padding([4, 8]);

        let tip = tooltip::tooltip(
            button,
            text::caption(self.panel_tooltip()),
            tooltip::Position::Bottom,
        );

        self.core.applet.autosize_window(tip).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        let panel = container(toaster(&self.toasts, self.popup_body()))
            .style(surface_style)
            .width(Length::Fill);

        autosize(panel, POPUP_AUTOSIZE_ID.clone())
            .limits(
                Limits::NONE
                    .min_width(POPUP_WIDTH)
                    .max_width(POPUP_WIDTH)
                    .min_height(1.0)
                    .max_height(760.0),
            )
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions =
            vec![time::every(Duration::from_secs(POLL_SECONDS)).map(|_| Message::Refresh)];
        if self.popup.is_some() && self.animating() {
            subscriptions.push(cosmic::iced::window::frames().map(|_| Message::Frame));
        }
        Subscription::batch(subscriptions)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl App {
    fn phase(&self) -> Phase {
        self.status.as_ref().map_or(Phase::Unknown, phase_of)
    }

    fn animating(&self) -> bool {
        self.status.as_ref().is_some_and(|status| {
            let target = status.capacity.unwrap_or(0) as f32;
            (target - self.capacity_view).abs() > 0.2
        })
    }

    fn accept_status(&mut self, mut status: Status, keep_local: bool) {
        if keep_local && (self.editing || self.saving) {
            if let Some(local) = &self.status {
                status.start = local.start;
                status.stop = local.stop;
            }
        }

        if self.last_update.is_none() || self.popup.is_none() {
            self.capacity_view = status.capacity.unwrap_or(0) as f32;
        }

        if let Some(capacity) = status.capacity {
            self.history.push_back(capacity.min(100) as u8);
            while self.history.len() > HISTORY_CAPACITY {
                self.history.pop_front();
            }
        }

        self.estimate = match phase_of(&status) {
            Phase::Charging => minutes_to_stop(status.capacity.unwrap_or(0), status.stop),
            _ => None,
        };

        self.status = Some(status);
        self.error = None;
        self.last_update = Some(Instant::now());
    }

    fn headline(&self) -> String {
        let Some(status) = &self.status else {
            return String::new();
        };

        match self.phase() {
            Phase::Off => "Charging is not limited".to_string(),
            Phase::OnBattery => {
                if status
                    .capacity
                    .is_some_and(|capacity| capacity <= status.start)
                {
                    "Charging resumes when you plug in".to_string()
                } else {
                    let buffer = status
                        .capacity
                        .unwrap_or(status.start)
                        .saturating_sub(status.start);
                    format!("{buffer}% above the resume point")
                }
            }
            Phase::Holding => format!("Resumes below {}%", status.start),
            Phase::Charging => match self.estimate {
                Some(minutes) => format!("About {minutes} min to {}%", status.stop),
                None => format!("Charging to {}%", status.stop),
            },
            Phase::Unknown => "Firmware state unavailable".to_string(),
        }
    }

    fn panel_tooltip(&self) -> String {
        match &self.status {
            Some(status) => {
                let capacity = status
                    .capacity
                    .map_or_else(|| "—".to_string(), |value| format!("{value}%"));
                if status.enabled {
                    format!(
                        "{} {capacity} · holds at {}% · resumes at {}%",
                        phase_word(self.phase()),
                        status.stop,
                        status.start
                    )
                } else {
                    format!("Charge limit off · {capacity}")
                }
            }
            None => "Charge limit · daemon unreachable".to_string(),
        }
    }

    fn popup_body(&self) -> Element<'_, Message> {
        let Some(status) = &self.status else {
            return self.offline_body();
        };

        let theme = cosmic::theme::active();
        let cosmic = theme.cosmic();
        let accent: Color = cosmic.accent_color().into();
        let warning: Color = cosmic.warning_color().into();
        let success: Color = cosmic.success_color().into();
        let on_bg: Color = cosmic.on_bg_color().into();
        let muted = move |alpha: f32| Color::from_rgba(on_bg.r, on_bg.g, on_bg.b, alpha);

        let phase = self.phase();
        let state_color = match phase {
            Phase::Charging => accent,
            Phase::Holding => success,
            Phase::OnBattery => muted(0.82),
            Phase::Off => muted(0.55),
            Phase::Unknown => muted(0.55),
        };

        let capacity = status
            .capacity
            .map_or_else(|| "—".to_string(), |value| format!("{value}%"));

        let glyph = canvas::Canvas::<PanelGlyph, Message, Theme>::new(PanelGlyph {
            capacity: status.capacity.map(|value| value as f32),
            stop: status.stop as f32,
            phase,
        })
        .width(Length::Fixed(38.0))
        .height(Length::Fixed(22.0));

        let chip = container(glyph)
            .padding([10, 9])
            .style(move |theme| tint_style(theme, state_color));

        let hero = container(
            column![
                row![
                    chip,
                    column![
                        text(capacity.clone()).size(30),
                        text::caption(phase_title(phase, status))
                            .size(12)
                            .class(cosmic::theme::Text::Color(state_color)),
                    ]
                    .spacing(0),
                    space::horizontal(),
                    column![
                        text::caption("WINDOW")
                            .size(10)
                            .class(cosmic::theme::Text::Color(muted(0.55))),
                        text(format!("{}–{}%", status.start, status.stop))
                            .size(15)
                            .class(cosmic::theme::Text::Color(muted(0.92))),
                    ]
                    .spacing(1)
                    .align_x(Alignment::End),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                text(self.headline())
                    .size(13)
                    .width(Length::Fill)
                    .class(cosmic::theme::Text::Color(muted(0.78))),
            ]
            .spacing(10),
        )
        .padding(14)
        .width(Length::Fill)
        .style(card_style);

        let history = container(
            column![
                row![
                    text("Last 20 minutes")
                        .size(12)
                        .class(cosmic::theme::Text::Color(muted(0.7))),
                    space::horizontal(),
                    text(format!("{capacity} now"))
                        .size(12)
                        .class(cosmic::theme::Text::Color(muted(0.7))),
                ],
                canvas::Canvas::<History<'_>, Message, Theme>::new(History {
                    samples: &self.history,
                    phase,
                    enabled: status.enabled,
                })
                .width(Length::Fill)
                .height(Length::Fixed(44.0)),
            ]
            .spacing(8),
        )
        .padding(14)
        .width(Length::Fill)
        .style(card_style);

        let presets = [
            ("Longevity", 60u32, 80u32),
            ("Balanced", 75, 80),
            ("Full", 95, 100),
        ];
        let mut preset_buttons: Vec<Element<'_, Message>> = Vec::with_capacity(3);
        for (label, start, stop) in presets {
            let active = status.start == start && status.stop == stop;
            preset_buttons.push(
                button::custom(text(label).size(12))
                    .class(if active {
                        cosmic::theme::Button::Suggested
                    } else {
                        cosmic::theme::Button::Standard
                    })
                    .width(Length::Fill)
                    .on_press(Message::ApplyPreset(start, stop))
                    .padding([7, 8])
                    .into(),
            );
        }
        let preset_row = row(preset_buttons).spacing(6);

        let window_card = container(
            column![
                row![
                    text("Charge window")
                        .size(12)
                        .class(cosmic::theme::Text::Color(muted(0.7))),
                    space::horizontal(),
                    text(format!("{}–{}%", status.start, status.stop))
                        .size(12)
                        .class(cosmic::theme::Text::Color(muted(0.9))),
                ],
                canvas::Canvas::<ChargeBand, Message, Theme>::new(ChargeBand {
                    charge: status.capacity.map(|_| self.capacity_view),
                    start: status.start as f32,
                    stop: status.stop as f32,
                    phase,
                    enabled: status.enabled,
                })
                .width(Length::Fill)
                .height(Length::Fixed(52.0)),
                preset_row,
            ]
            .spacing(10),
        )
        .padding(14)
        .width(Length::Fill)
        .style(card_style);

        let limit_card = container(
            row![
                column![
                    text("Charge limit").size(13),
                    text::caption(if status.enabled {
                        format!("Stops at {}% · resumes at {}%", status.stop, status.start)
                    } else {
                        "Off · firmware charges normally".to_string()
                    })
                    .size(12)
                    .class(cosmic::theme::Text::Color(muted(0.65))),
                ]
                .spacing(2),
                space::horizontal(),
                toggler(status.enabled).on_toggle(Message::ToggleLimit),
            ]
            .align_y(Alignment::Center),
        )
        .padding(14)
        .width(Length::Fill)
        .style(card_style);

        let advanced_button = button::custom(
            row![
                text("Advanced").size(13),
                space::horizontal(),
                text(if self.show_advanced { "Hide" } else { "Sliders" })
                    .size(12)
                    .class(cosmic::theme::Text::Color(muted(0.6))),
                text(if self.show_advanced { "▾" } else { "▸" })
                    .size(12)
                    .class(cosmic::theme::Text::Color(muted(0.6))),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .class(cosmic::theme::Button::Standard)
        .width(Length::Fill)
        .padding([10, 14])
        .on_press(Message::ToggleAdvanced);

        let mut body = column![hero, history, window_card, limit_card, advanced_button].spacing(10);

        if self.show_advanced {
            let stop_min = (status.start + 1).min(100) as f64;
            let stop_value = (status.stop as f64).clamp(stop_min, 100.0);
            let start_max = status.stop.saturating_sub(1) as f64;
            let start_value = (status.start as f64).clamp(0.0, start_max.max(0.0));

            let slider_header = |title: &'static str, value: u32| {
                row![
                    text(title).size(13),
                    space::horizontal(),
                    text(format!("{value}%"))
                        .size(14)
                        .class(cosmic::theme::Text::Color(muted(0.95))),
                ]
                .align_y(Alignment::Center)
            };

            let advanced = container(
                column![
                    slider_header("Stop at", status.stop),
                    slider(stop_min..=100.0, stop_value, Message::StopChanged)
                        .step(1.0)
                        .width(Length::Fill)
                        .on_release(Message::Persist),
                    text::caption(format!("Range {}–100%", stop_min as u32))
                        .size(11)
                        .class(cosmic::theme::Text::Color(muted(0.55))),
                    slider_header("Resume at", status.start),
                    slider(0.0..=start_max.max(0.0), start_value, Message::StartChanged)
                        .step(1.0)
                        .width(Length::Fill)
                        .on_release(Message::Persist),
                    text::caption(format!("Range 0–{}%", start_max.max(0.0) as u32))
                        .size(11)
                        .class(cosmic::theme::Text::Color(muted(0.55))),
                ]
                .spacing(8),
            )
            .padding(14)
            .width(Length::Fill)
            .style(card_style);

            body = body.push(advanced);
        }

        body = body.push(self.footer_row(status, muted));
        if let Some(error) = &self.error {
            body = body.push(
                container(
                    row![
                        text("⚠").size(14).class(cosmic::theme::Text::Color(warning)),
                        text::caption(error.clone())
                            .size(12)
                            .width(Length::Fill)
                            .class(cosmic::theme::Text::Color(warning)),
                        button::text("Retry").on_press(Message::Refresh),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding(12)
                .width(Length::Fill)
                .style(move |theme| tint_style(theme, warning)),
            );
        }

        container(body).padding(16).width(Length::Fill).into()
    }

    fn footer_row<'a>(
        &'a self,
        status: &'a Status,
        muted: impl Fn(f32) -> Color + 'a,
    ) -> Element<'a, Message> {
        let note = if self.saving {
            "Saving…".to_string()
        } else {
            match self.last_update {
                Some(update) => {
                    let seconds = update.elapsed().as_secs();
                    if seconds < 2 {
                        "Updated just now".to_string()
                    } else {
                        format!("Updated {seconds}s ago")
                    }
                }
                None => "Waiting for the daemon".to_string(),
            }
        };

        let firmware = match (status.mode, status.mode_name.as_deref()) {
            (Some(mode), Some(name)) => format!(
                "Firmware: {name} (0x{mode:02x})\nBattery: {}\nSocket: {}",
                status.status,
                socket::SOCKET_PATH
            ),
            (Some(mode), None) => format!(
                "Firmware: 0x{mode:02x}\nBattery: {}\nSocket: {}",
                status.status,
                socket::SOCKET_PATH
            ),
            _ => format!(
                "Firmware state unavailable\nBattery: {}\nSocket: {}",
                status.status,
                socket::SOCKET_PATH
            ),
        };

        row![
            text::caption(note)
                .size(11)
                .class(cosmic::theme::Text::Color(muted(0.6))),
            space::horizontal(),
            tooltip::tooltip(
                text::caption(format!("Checks every {}s", status.interval))
                    .size(11)
                    .class(cosmic::theme::Text::Color(muted(0.6))),
                text::caption(firmware),
                tooltip::Position::Top,
            ),
        ]
        .align_y(Alignment::Center)
        .into()
    }

    fn offline_body(&self) -> Element<'_, Message> {
        let theme = cosmic::theme::active();
        let cosmic = theme.cosmic();
        let warning: Color = cosmic.warning_color().into();
        let on_bg: Color = cosmic.on_bg_color().into();
        let muted = Color::from_rgba(on_bg.r, on_bg.g, on_bg.b, 0.72);

        let mut body = column![
            row![
                text("⚠")
                    .size(22)
                    .class(cosmic::theme::Text::Color(warning)),
                text::title4("Charge limit unavailable"),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(
                "The background service is not running, so the battery cannot be read or limited."
            )
            .size(13)
            .class(cosmic::theme::Text::Color(muted)),
            row![
                button::standard("Retry").on_press(Message::Refresh),
                button::text("Copy command").on_press(Message::CopyCommand),
            ]
            .spacing(8),
            text::monotext(SERVICE_COMMAND).class(cosmic::theme::Text::Color(muted)),
        ]
        .spacing(10);

        if let Some(error) = &self.error {
            body = body.push(
                text::caption(error.clone())
                    .size(12)
                    .class(cosmic::theme::Text::Color(warning)),
            );
        }

        container(
            container(body)
                .padding(14)
                .width(Length::Fill)
                .style(card_style),
        )
        .padding(16)
        .width(Length::Fill)
        .into()
    }
}

fn phase_of(status: &Status) -> Phase {
    if !status.enabled {
        return Phase::Off;
    }
    if !status.ac {
        return Phase::OnBattery;
    }
    if status.mode == Some(0x04)
        || status
            .capacity
            .is_some_and(|capacity| capacity >= status.stop)
    {
        Phase::Holding
    } else {
        Phase::Charging
    }
}

fn phase_title(phase: Phase, status: &Status) -> String {
    match phase {
        Phase::Charging => "Charging".to_string(),
        Phase::Holding => format!("Holding at {}%", status.stop),
        Phase::OnBattery => "On battery".to_string(),
        Phase::Off => "Off".to_string(),
        Phase::Unknown => "Unknown".to_string(),
    }
}

fn phase_word(phase: Phase) -> &'static str {
    match phase {
        Phase::Charging => "Charging",
        Phase::Holding => "Holding",
        Phase::OnBattery => "On battery",
        Phase::Off => "Off",
        Phase::Unknown => "Unknown",
    }
}

fn set_request(start: u32, stop: u32) -> String {
    format!(r#"{{"cmd":"set","start":{start},"stop":{stop}}}"#)
}

fn short_error(error: &str) -> String {
    let line = error.lines().next().unwrap_or(error).trim();
    if line.chars().count() > 64 {
        format!("{}…", line.chars().take(63).collect::<String>())
    } else {
        line.to_string()
    }
}

fn read_u64(path: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn minutes_to_stop(capacity: u32, stop: u32) -> Option<u32> {
    if stop <= capacity {
        return None;
    }
    let full = read_u64(&format!("{BATTERY}/energy_full"))?;
    let power = read_u64(&format!("{BATTERY}/power_now"))?;
    if full == 0 || power == 0 {
        return None;
    }
    let seconds = (stop - capacity) as f64 / 100.0 * full as f64 / power as f64 * 3600.0;
    Some(((seconds / 60.0).round() as u32).max(1))
}

fn muted(theme: &Theme, alpha: f32) -> Color {
    let on: Color = theme.cosmic().on_bg_color().into();
    Color::from_rgba(on.r, on.g, on.b, alpha)
}

fn phase_color(theme: &Theme, phase: Phase) -> Color {
    match phase {
        Phase::Charging => theme.cosmic().accent_color().into(),
        Phase::Holding => theme.cosmic().success_color().into(),
        Phase::OnBattery => muted(theme, 0.82),
        Phase::Off => muted(theme, 0.55),
        Phase::Unknown => muted(theme, 0.5),
    }
}

// ---------------------------------------------------------------------------
// Surface styling
// ---------------------------------------------------------------------------

fn base_color(theme: &Theme) -> Color {
    theme.cosmic().background(theme.transparent).base.into()
}

fn on_color(theme: &Theme) -> Color {
    theme.cosmic().background(theme.transparent).on.into()
}

/// Foreground color at a given opacity over the themed background.
fn on_alpha(theme: &Theme, alpha: f32) -> Color {
    let on = on_color(theme);
    Color::from_rgba(on.r, on.g, on.b, alpha)
}

/// The popup shell: a soft vertical gradient with a hairline edge and a deep
/// shadow. Opacity follows the theme's frosted setting so the applet never
/// collapses into a flat opaque slab.
fn surface_style(theme: &Theme) -> cosmic::iced::widget::container::Style {
    let base = base_color(theme);
    let (top, middle, bottom) = if theme.cosmic().frosted_applets {
        (0.44, 0.54, 0.64)
    } else {
        (0.86, 0.92, 0.97)
    };

    let background = Background::Gradient(Gradient::Linear(
        gradient::Linear::new(std::f32::consts::PI)
            .add_stop(0.0, Color::from_rgba(base.r, base.g, base.b, top))
            .add_stop(
                0.5,
                Color::from_rgba(base.r * 0.97, base.g * 0.97, base.b * 0.97, middle),
            )
            .add_stop(
                1.0,
                Color::from_rgba(base.r * 0.94, base.g * 0.94, base.b * 0.94, bottom),
            ),
    ));

    cosmic::iced::widget::container::Style {
        background: Some(background),
        border: Border {
            radius: RADIUS.into(),
            width: 1.0,
            color: on_alpha(theme, 0.1),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: Vector::new(0.0, 18.0),
            blur_radius: 44.0,
        },
        text_color: Some(on_color(theme)),
        icon_color: Some(on_color(theme)),
        ..Default::default()
    }
}

/// A raised card: lifted slightly toward the foreground so sections read as
/// distinct blocks without dividers.
fn card_style(theme: &Theme) -> cosmic::iced::widget::container::Style {
    let base = base_color(theme);
    let on = on_color(theme);
    let lift = 0.07;
    let r = base.r + (on.r - base.r) * lift;
    let g = base.g + (on.g - base.g) * lift;
    let b = base.b + (on.b - base.b) * lift;

    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(Color::from_rgba(r, g, b, 0.72))),
        border: Border {
            radius: RADIUS.into(),
            width: 1.0,
            color: on_alpha(theme, 0.1),
        },
        text_color: Some(on),
        icon_color: Some(on),
        ..Default::default()
    }
}

/// A tinted chip used behind status glyphs and error rows.
fn tint_style(theme: &Theme, color: Color) -> cosmic::iced::widget::container::Style {
    let _ = theme;
    cosmic::iced::widget::container::Style {
        background: Some(Background::Color(Color::from_rgba(
            color.r, color.g, color.b, 0.16,
        ))),
        border: Border {
            radius: 10.0.into(),
            width: 1.0,
            color: Color::from_rgba(color.r, color.g, color.b, 0.3),
        },
        ..Default::default()
    }
}

fn fetch() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| socket::call(r#"{"cmd":"status"}"#))
                .await
                .map_err(|error| error.to_string())?
        },
        |result| cosmic::Action::App(Message::Loaded(result)),
    )
}

fn send(request: String) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || socket::call(&request))
                .await
                .map_err(|error| error.to_string())?
        },
        |result| cosmic::Action::App(Message::Saved(result)),
    )
}

// ---------------------------------------------------------------------------
// Panel glyph: a compact charge bar with a notch marking the charge limit.
// ---------------------------------------------------------------------------

struct PanelGlyph {
    capacity: Option<f32>,
    stop: f32,
    phase: Phase,
}

impl canvas::Program<Message, Theme> for PanelGlyph {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        let width = bounds.width;
        let height = bounds.height;
        let track_height = height.min(9.0);
        let y = (height - track_height) / 2.0;
        let radius = track_height / 2.0;

        let track = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                Point::new(0.0, y),
                Size::new(width, track_height),
                radius.into(),
            )
        });
        frame.fill(&track, muted(theme, 0.18));

        if let Some(capacity) = self.capacity {
            let fill_width = (width * (capacity / 100.0).clamp(0.0, 1.0)).max(2.0);
            let fill = canvas::Path::new(|builder| {
                builder.rounded_rectangle(
                    Point::new(0.0, y),
                    Size::new(fill_width, track_height),
                    radius.min(fill_width / 2.0).into(),
                )
            });
            frame.fill(&fill, phase_color(theme, self.phase));
        }

        let notch_x = (width * (self.stop / 100.0).clamp(0.0, 1.0)).clamp(1.5, width - 1.5);
        let notch = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                Point::new(notch_x - 1.25, y - 3.0),
                Size::new(2.5, track_height + 6.0),
                1.25.into(),
            )
        });
        // Punch the notch out in the surface color: a light mark would vanish
        // against a nearly full accent fill.
        let base = base_color(theme);
        frame.fill(&notch, Color::from_rgba(base.r, base.g, base.b, 0.95));

        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------------------------
// History: the last 20 minutes of charge, drawn as a small area chart.
// ---------------------------------------------------------------------------

struct History<'a> {
    samples: &'a VecDeque<u8>,
    phase: Phase,
    enabled: bool,
}

impl canvas::Program<Message, Theme> for History<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &cosmic::iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        let width = bounds.width;
        let height = bounds.height;
        let pad = 3.0;
        let inner = (height - pad * 2.0).max(1.0);
        let color = if self.enabled {
            phase_color(theme, self.phase)
        } else {
            muted(theme, 0.55)
        };
        let len = self.samples.len();
        if len == 0 {
            return vec![frame.into_geometry()];
        }

        // Scale to the observed range instead of 0-100%. A flat 79% series
        // would otherwise render as a solid block pinned to the top edge.
        let min = *self.samples.iter().min().unwrap_or(&0) as f32;
        let max = *self.samples.iter().max().unwrap_or(&0) as f32;
        let spread = (max - min).max(1.0);
        let margin = (spread * 0.35).max(1.0);
        let low = (min - margin).clamp(0.0, 100.0);
        let high = (max + margin).clamp(0.0, 100.0);
        let range = (high - low).max(1.0);

        let point = |index: usize, value: u8| -> Point {
            let x = if len <= 1 {
                width / 2.0
            } else {
                width * index as f32 / (len - 1) as f32
            };
            let ratio = ((value as f32 - low) / range).clamp(0.0, 1.0);
            Point::new(x, pad + inner * (1.0 - ratio))
        };

        // Faint guides keep the compact chart feeling measured.
        for step in [0.0f32, 0.5, 1.0] {
            let y = pad + inner * step;
            let guide = canvas::Path::new(|builder| {
                builder.move_to(Point::new(0.0, y));
                builder.line_to(Point::new(width, y));
            });
            frame.stroke(
                &guide,
                canvas::Stroke::default()
                    .with_width(1.0)
                    .with_color(on_alpha(theme, 0.08)),
            );
        }

        let area = canvas::Path::new(|builder| {
            builder.move_to(Point::new(0.0, pad + inner));
            for (index, value) in self.samples.iter().enumerate() {
                builder.line_to(point(index, *value));
            }
            builder.line_to(Point::new(width, pad + inner));
            builder.close();
        });
        frame.fill(
            &area,
            Color::from_rgba(color.r, color.g, color.b, 0.18),
        );

        if len == 1 {
            let dot = canvas::Path::circle(point(0, self.samples[0]), 2.5);
            frame.fill(&dot, color);
        } else {
            let line = canvas::Path::new(|builder| {
                for (index, value) in self.samples.iter().enumerate() {
                    let position = point(index, *value);
                    if index == 0 {
                        builder.move_to(position);
                    } else {
                        builder.line_to(position);
                    }
                }
            });
            frame.stroke(
                &line,
                canvas::Stroke::default()
                    .with_width(2.0)
                    .with_color(Color::from_rgba(color.r, color.g, color.b, 0.95)),
            );

            // Endpoint dot anchors the series to "now".
            let last = len - 1;
            let dot = canvas::Path::circle(point(last, self.samples[last]), 3.0);
            frame.fill(&dot, color);
        }

        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------------------------
// Charge band: current charge plus a draggable resume -> stop window.
// ---------------------------------------------------------------------------

struct ChargeBand {
    charge: Option<f32>,
    start: f32,
    stop: f32,
    phase: Phase,
    enabled: bool,
}

#[derive(Default)]
struct BandState {
    drag: Option<Drag>,
    hover: Option<Drag>,
}

impl ChargeBand {
    fn handle_at(&self, position: Point, width: f32) -> Option<Drag> {
        let start_x = width * (self.start / 100.0).clamp(0.0, 1.0);
        let stop_x = width * (self.stop / 100.0).clamp(0.0, 1.0);
        let start_distance = (position.x - start_x).abs();
        let stop_distance = (position.x - stop_x).abs();
        let threshold = 14.0;

        match (start_distance <= threshold, stop_distance <= threshold) {
            (true, true) => {
                if start_distance <= stop_distance {
                    Some(Drag::Start)
                } else {
                    Some(Drag::Stop)
                }
            }
            (true, false) => Some(Drag::Start),
            (false, true) => Some(Drag::Stop),
            _ => None,
        }
    }
}

fn value_at(x: f32, width: f32) -> u32 {
    (((x / width.max(1.0)).clamp(0.0, 1.0)) * 100.0).round() as u32
}

impl canvas::Program<Message, Theme> for ChargeBand {
    type State = BandState;

    fn update(
        &self,
        state: &mut BandState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(position) = cursor.position_in(bounds) {
                    if let Some(handle) = self.handle_at(position, bounds.width) {
                        state.drag = Some(handle);
                        return Some(canvas::Action::capture());
                    }
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(handle) = state.drag {
                    if let Some(position) = cursor.position_in(bounds) {
                        let value = value_at(position.x, bounds.width);
                        return Some(
                            canvas::Action::publish(Message::BandDragged(handle, value))
                                .and_capture(),
                        );
                    }
                } else {
                    let hover = cursor
                        .position_in(bounds)
                        .and_then(|position| self.handle_at(position, bounds.width));
                    if hover != state.hover {
                        state.hover = hover;
                    }
                    return Some(canvas::Action::request_redraw());
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if state.drag.take().is_some() {
                    return Some(
                        canvas::Action::publish(Message::CommitBand).and_capture(),
                    );
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorLeft) => {
                state.hover = None;
                return Some(canvas::Action::request_redraw());
            }
            _ => {}
        }
        None
    }

    fn mouse_interaction(
        &self,
        state: &BandState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.drag.is_some() {
            return mouse::Interaction::Grabbing;
        }
        match cursor
            .position_in(bounds)
            .and_then(|position| self.handle_at(position, bounds.width))
        {
            Some(_) => mouse::Interaction::Grab,
            None => mouse::Interaction::default(),
        }
    }

    fn draw(
        &self,
        state: &BandState,
        renderer: &cosmic::iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        let width = bounds.width;
        let track_y = 15.0;
        let track_height = 10.0;
        let radius = track_height / 2.0;
        let color = phase_color(theme, self.phase);
        let edge = if self.enabled {
            color
        } else {
            muted(theme, 0.5)
        };

        let track = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                Point::new(0.0, track_y),
                Size::new(width, track_height),
                radius.into(),
            )
        });
        frame.fill(&track, on_alpha(theme, 0.14));

        let start_x = width * (self.start / 100.0).clamp(0.0, 1.0);
        let stop_x = width * (self.stop / 100.0).clamp(0.0, 1.0);
        let (low, high) = if start_x <= stop_x {
            (start_x, stop_x)
        } else {
            (stop_x, start_x)
        };
        let zone_width = (high - low).max(1.0);

        // Everything past the stop point is blocked; dim it so the cap reads
        // even when the charge marker sits right next to it.
        if self.stop < 100.0 {
            let blocked = canvas::Path::new(|builder| {
                builder.rounded_rectangle(
                    Point::new(stop_x, track_y),
                    Size::new((width - stop_x).max(1.0), track_height),
                    radius.into(),
                )
            });
            frame.fill(&blocked, on_alpha(theme, 0.06));
        }

        let zone = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                Point::new(low, track_y),
                Size::new(zone_width, track_height),
                radius.min(zone_width / 2.0).into(),
            )
        });
        frame.fill(
            &zone,
            Color::from_rgba(
                color.r,
                color.g,
                color.b,
                if self.enabled { 0.32 } else { 0.16 },
            ),
        );
        frame.stroke(
            &zone,
            canvas::Stroke::default()
                .with_width(1.0)
                .with_color(Color::from_rgba(
                    color.r,
                    color.g,
                    color.b,
                    if self.enabled { 0.9 } else { 0.4 },
                )),
        );

        if let Some(charge) = self.charge {
            let x = (width * (charge / 100.0).clamp(0.0, 1.0)).clamp(1.0, width - 1.0);
            let tick = canvas::Path::new(|builder| {
                builder.rectangle(
                    Point::new(x - 0.75, track_y - 3.5),
                    Size::new(1.5, track_height + 7.0),
                )
            });
            frame.fill(&tick, on_alpha(theme, if self.enabled { 0.85 } else { 0.5 }));
        }

        for percent in [0u32, 25, 50, 75, 100] {
            let x = width * percent as f32 / 100.0;
            let tick = canvas::Path::new(|builder| {
                builder.rectangle(
                    Point::new((x - 0.5).max(0.0), track_y + track_height + 2.0),
                    Size::new(1.0, 4.0),
                )
            });
            frame.fill(&tick, on_alpha(theme, 0.35));

            let align = match percent {
                0 => Alignment::Start,
                100 => Alignment::End,
                _ => Alignment::Center,
            };
            let _ = frame.fill_text(canvas::Text {
                content: percent.to_string(),
                position: Point::new(x, track_y + track_height + 8.0),
                color: on_alpha(theme, 0.62),
                size: 10.0.into(),
                align_x: align.into(),
                align_y: alignment::Vertical::Top,
                ..Default::default()
            });
        }

        let base = base_color(theme);
        for (value, handle) in [(self.start, Drag::Start), (self.stop, Drag::Stop)] {
            let x = (width * (value / 100.0).clamp(0.0, 1.0)).clamp(7.0, width - 7.0);
            let active = state.drag == Some(handle) || state.hover == Some(handle);
            let handle_radius = if active { 8.0 } else { 6.5 };
            let center = Point::new(x, track_y + track_height / 2.0);

            if active {
                let halo = canvas::Path::circle(center, handle_radius + 4.0);
                frame.fill(&halo, Color::from_rgba(edge.r, edge.g, edge.b, 0.18));
            }

            let knob = canvas::Path::circle(center, handle_radius);
            frame.fill(&knob, Color::from_rgba(base.r, base.g, base.b, 0.96));
            frame.stroke(
                &knob,
                canvas::Stroke::default().with_width(2.0).with_color(edge),
            );
        }

        vec![frame.into_geometry()]
    }
}
