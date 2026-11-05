//! COSMIC panel applet for the HP charge limiter daemon.
//!
//! The applet only talks to the root daemon over `/run/hp-charge-limit.sock`;
//! it never touches ACPI itself.

use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::iced::core::window;
use cosmic::iced::mouse;
use cosmic::iced::platform_specific::shell::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::time;
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, Color, Length, Limits, Rectangle, Subscription};
use cosmic::widget::{button, canvas, column, divider, row, settings, slider, space, text, toggler};
use cosmic::{Element, Theme};

use crate::socket::{self, Status};

const ID: &str = "io.github.AceMythos.HpCosmicChargeLimit";

pub struct App {
    core: Core,
    popup: Option<Id>,
    status: Option<Status>,
    error: Option<String>,
    /// True while a slider is being dragged so the poll cannot move it.
    editing: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            core: Core::default(),
            popup: None,
            status: None,
            error: None,
            editing: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    Refresh,
    Loaded(Result<Status, String>),
    ToggleLimit(bool),
    StopChanged(f64),
    StartChanged(f64),
    Persist,
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
                        .min_height(320.0)
                        .max_height(800.0);

                    Task::batch([get_popup(popup_settings), fetch()])
                };
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
            Message::Refresh => return fetch(),
            Message::Loaded(result) => match result {
                Ok(mut status) => {
                    if self.editing {
                        // Keep whatever the user is currently dragging.
                        if let Some(local) = &self.status {
                            status.start = local.start;
                            status.stop = local.stop;
                        }
                    }
                    self.status = Some(status);
                    self.error = None;
                }
                Err(error) => {
                    // A single failed poll should not blank a working popup.
                    self.error = Some(error);
                }
            },
            Message::ToggleLimit(enabled) => {
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
                if let Some(status) = self.status.clone() {
                    self.editing = false;
                    return send(format!(
                        r#"{{"cmd":"set","start":{},"stop":{}}}"#,
                        status.start, status.stop
                    ));
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let active = self.status.as_ref().is_some_and(|status| status.enabled);
        let label = match &self.status {
            Some(status) if status.enabled => format!("{}%", status.stop),
            Some(_) => "off".to_string(),
            None => "—".to_string(),
        };

        let glyph = canvas::Canvas::<LimitGlyph, Message, Theme>::new(LimitGlyph { active })
            .width(Length::Fixed(22.0))
            .height(Length::Fixed(16.0));

        let content = row![glyph, text::body(label)].spacing(6).align_y(Alignment::Center);

        let btn = button::custom(content)
            .class(cosmic::theme::Button::AppletIcon)
            .on_press_down(Message::TogglePopup)
            .padding([4, 8]);

        self.core.applet.autosize_window(btn).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        self.core.applet.popup_container(self.popup_body()).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        time::every(Duration::from_secs(2)).map(|_| Message::Refresh)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl App {
    fn popup_body(&self) -> Element<'_, Message> {
        let Some(status) = &self.status else {
            let mut body = column![
                text::title4("Charge limit"),
                text("The daemon is not running."),
            ]
            .spacing(8);

            if let Some(error) = &self.error {
                body = body.push(text::caption(error.clone()));
            }

            body = body.push(button::standard("Retry").on_press(Message::Refresh));
            body = body.push(text::caption("sudo systemctl start hp-charge-limit"));

            return column![body].padding(16).width(Length::Fill).into();
        };

        let capacity = status
            .capacity
            .map(|c| format!("{c}%"))
            .unwrap_or_else(|| "—".into());
        let mode_label = match status.mode {
            Some(0) => "auto".to_string(),
            Some(4) => "inhibit".to_string(),
            Some(mode) => format!("0x{mode:02x}"),
            None => "unavailable".to_string(),
        };

        let theme = cosmic::theme::active();
        let accent: Color = theme.cosmic().accent_color().into();
        let on_bg: Color = theme.cosmic().on_bg_color().into();
        let muted_color = Color::from_rgba(on_bg.r, on_bg.g, on_bg.b, 0.55);

        let state_text = if status.enabled { "Limiting" } else { "Off" };
        let state_color = if status.enabled { accent } else { muted_color };

        let header_glyph =
            canvas::Canvas::<LimitGlyph, Message, Theme>::new(LimitGlyph { active: status.enabled })
                .width(Length::Fixed(34.0))
                .height(Length::Fixed(26.0));

        let header = row![
            header_glyph,
            column![
                text(capacity).size(30),
                text::caption(format!(
                    "{} · {}",
                    status.status,
                    if status.ac { "AC online" } else { "on battery" }
                )),
            ]
            .spacing(0)
            .width(Length::Fill),
            column![
                text(state_text).size(14).class(cosmic::theme::Text::Color(state_color)),
                text::caption(format!("{}-{}%", status.start, status.stop)),
            ]
            .spacing(1)
            .align_x(Alignment::End),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let band = canvas::Canvas::<ChargeBand, Message, Theme>::new(ChargeBand {
            charge: status.capacity.map(|c| c as f32),
            start: status.start as f32,
            stop: status.stop as f32,
            active: status.enabled,
        })
        .width(Length::Fill)
        .height(Length::Fixed(18.0));

        let band_caption = text::caption(format!(
            "resumes at {}% · stops at {}%",
            status.start, status.stop
        ));

        let stop_min = (status.start + 1).min(100) as f64;
        let stop_value = (status.stop as f64).clamp(stop_min, 100.0);
        let stop_row = row![
            text("Stop charging at"),
            space::horizontal(),
            text(format!("{}%", status.stop)).size(16),
        ]
        .align_y(Alignment::Center);

        let start_max = status.stop.saturating_sub(1) as f64;
        let start_value = (status.start as f64).clamp(0.0, start_max.max(0.0));
        let start_row = row![
            text("Resume charging at"),
            space::horizontal(),
            text(format!("{}%", status.start)).size(16),
        ]
        .align_y(Alignment::Center);

        let mut body = column![
            header,
            band,
            band_caption,
            divider::horizontal::default(),
            settings::item(
                "Charge limit",
                toggler(status.enabled).on_toggle(Message::ToggleLimit),
            ),
            divider::horizontal::default(),
            stop_row,
            slider(stop_min..=100.0, stop_value, Message::StopChanged)
                .step(1.0)
                .width(Length::Fill)
                .on_release(Message::Persist),
            start_row,
            slider(0.0..=start_max.max(0.0), start_value, Message::StartChanged)
                .step(1.0)
                .width(Length::Fill)
                .on_release(Message::Persist),
            divider::horizontal::default(),
            text::caption(format!(
                "firmware {} · polled every {} s",
                mode_label, status.interval
            )),
        ]
        .spacing(10)
        .padding(16);

        if let Some(error) = &self.error {
            body = body.push(text::caption(error.clone()).class(cosmic::theme::Text::Color(muted_color)));
        }

        body.width(Length::Fill).into()
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
        |result| cosmic::Action::App(Message::Loaded(result)),
    )
}

// ---------------------------------------------------------------------------
// Custom glyph: a lightning bolt under a ceiling bar. No battery outline.
// ---------------------------------------------------------------------------

struct LimitGlyph {
    active: bool,
}

impl canvas::Program<Message, Theme> for LimitGlyph {
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

        let w = bounds.width;
        let h = bounds.height;
        let accent: Color = theme.cosmic().accent_color().into();
        let on: Color = theme.cosmic().on_bg_color().into();
        let muted = Color::from_rgba(on.r, on.g, on.b, 0.40);

        let cap_height = (h * 0.13).clamp(1.5, 3.0);
        let cap = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                cosmic::iced::Point::new(w * 0.10, 0.0),
                cosmic::iced::Size::new(w * 0.80, cap_height),
                cosmic::iced::border::Radius::from(cap_height / 2.0),
            )
        });
        frame.fill(&cap, if self.active { accent } else { muted });

        let bolt = canvas::Path::new(|builder| {
            builder.move_to(cosmic::iced::Point::new(w * 0.58, h * 0.24));
            builder.line_to(cosmic::iced::Point::new(w * 0.28, h * 0.60));
            builder.line_to(cosmic::iced::Point::new(w * 0.47, h * 0.60));
            builder.line_to(cosmic::iced::Point::new(w * 0.40, h * 0.94));
            builder.line_to(cosmic::iced::Point::new(w * 0.73, h * 0.52));
            builder.line_to(cosmic::iced::Point::new(w * 0.54, h * 0.52));
            builder.close();
        });
        frame.fill(&bolt, if self.active { on } else { muted });

        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------------------------
// Charge band: current charge plus the resume -> stop window.
// ---------------------------------------------------------------------------

struct ChargeBand {
    charge: Option<f32>,
    start: f32,
    stop: f32,
    active: bool,
}

impl canvas::Program<Message, Theme> for ChargeBand {
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

        let w = bounds.width;
        let track_h = 12.0;
        let y = (bounds.height - track_h) / 2.0;
        let radius = track_h / 2.0;

        let accent: Color = theme.cosmic().accent_color().into();
        let on: Color = theme.cosmic().on_bg_color().into();
        let track_color = Color::from_rgba(on.r, on.g, on.b, 0.16);

        let track = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                cosmic::iced::Point::new(0.0, y),
                cosmic::iced::Size::new(w, track_h),
                cosmic::iced::border::Radius::from(radius),
            )
        });
        frame.fill(&track, track_color);

        let start = (self.start / 100.0).clamp(0.0, 1.0);
        let stop = (self.stop / 100.0).clamp(0.0, 1.0);
        let (lo, hi) = if start <= stop {
            (start, stop)
        } else {
            (stop, start)
        };

        let zone_x = w * lo;
        let zone_w = (w * (hi - lo)).max(1.0);
        let zone_color = if self.active {
            Color::from_rgba(accent.r, accent.g, accent.b, 0.50)
        } else {
            Color::from_rgba(on.r, on.g, on.b, 0.20)
        };
        let zone_edge = if self.active {
            Color::from_rgba(accent.r, accent.g, accent.b, 0.95)
        } else {
            Color::from_rgba(on.r, on.g, on.b, 0.45)
        };
        let zone = canvas::Path::new(|builder| {
            builder.rounded_rectangle(
                cosmic::iced::Point::new(zone_x, y),
                cosmic::iced::Size::new(zone_w, track_h),
                cosmic::iced::border::Radius::from(radius.min(zone_w / 2.0)),
            )
        });
        frame.fill(&zone, zone_color);
        frame.stroke(
            &zone,
            canvas::Stroke::default()
                .with_width(1.0)
                .with_color(zone_edge),
        );

        if let Some(charge) = self.charge {
            let marker_x = (w * (charge / 100.0).clamp(0.0, 1.0)).clamp(2.0, w - 2.0);
            let marker = canvas::Path::new(|builder| {
                builder.rounded_rectangle(
                    cosmic::iced::Point::new(marker_x - 1.5, y - 3.0),
                    cosmic::iced::Size::new(3.0, track_h + 6.0),
                    cosmic::iced::border::Radius::from(1.5),
                )
            });
            frame.fill(&marker, if self.active { on } else { accent });
        }

        vec![frame.into_geometry()]
    }
}
