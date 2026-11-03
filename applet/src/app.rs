//! COSMIC panel applet for the HP charge limiter daemon.
//!
//! The applet only talks to the root daemon over `/run/hp-charge-limit.sock`;
//! it never touches ACPI itself.

use std::time::Duration;

use cosmic::app::{Core, Task};
use cosmic::iced::core::window;
use cosmic::iced::time;
use cosmic::iced::window::Id;
use cosmic::iced::{Length, Limits, Rectangle, Subscription};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{column, divider, list_column, row, settings, slider, text, toggler};
use cosmic::Element;

use crate::socket::{self, Status};

const ID: &str = "io.github.AceMythos.HpCosmicChargeLimit";

pub struct App {
    core: Core,
    popup: Option<Id>,
    status: Option<Status>,
    error: Option<String>,
    /// True while a slider is being dragged so the 2 s poll cannot move it.
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
    Surface(cosmic::surface::Action<Message>),
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
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Surface(action));
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
                    self.status = None;
                    self.error = Some(error);
                }
            },
            Message::ToggleLimit(enabled) => {
                return send(format!(r#"{{"cmd":"enable","value":{enabled}}}"#));
            }
            Message::StopChanged(value) => {
                if let Some(status) = &mut self.status {
                    status.stop = (value.round() as u32)
                        .min(100)
                        .max(status.start.saturating_add(1));
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
        let have_popup = self.popup;
        let btn = self
            .core
            .applet
            .icon_button(if self.status.as_ref().is_some_and(|status| status.enabled) {
                "battery-level-80-symbolic"
            } else {
                "battery-level-100-symbolic"
            })
            .on_press_with_rectangle(move |offset, bounds| {
                if let Some(id) = have_popup {
                    Message::Surface(destroy_popup(id))
                } else {
                    Message::Surface(app_popup::<App>(
                        |_| Default::default(),
                        move |state: &mut App| {
                            let new_id = Id::unique();
                            state.popup = Some(new_id);
                            let mut popup_settings = state.core.applet.get_popup_settings(
                                state.core.main_window_id().unwrap(),
                                new_id,
                                None,
                                None,
                                None,
                            );
                            popup_settings.positioner.anchor_rect = Rectangle {
                                x: (bounds.x - offset.x) as i32,
                                y: (bounds.y - offset.y) as i32,
                                width: bounds.width as i32,
                                height: bounds.height as i32,
                            };
                            popup_settings.positioner.size_limits = Limits::NONE
                                .max_width(400.0)
                                .min_width(320.0)
                                .min_height(240.0)
                                .max_height(800.0);
                            popup_settings
                        },
                        Some(Box::new(|state: &App| {
                            Element::from(state.core.applet.popup_container(state.popup_body()))
                                .map(cosmic::Action::App)
                        })),
                    ))
                }
            });

        let tooltip = match &self.status {
            Some(status) => format!(
                "{}% · limit {} · {}",
                status
                    .capacity
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "--".into()),
                if status.enabled { "on" } else { "off" },
                status.mode_name
            ),
            None => "Charge limiter daemon not reachable".to_string(),
        };

        Element::from(self.core.applet.applet_tooltip::<Message>(
            btn,
            tooltip,
            self.popup.is_some(),
            |action| Message::Surface(action),
            None,
        ))
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
            let mut body = list_column().list_item_padding(8);
            body = body.add(text::title4("Charge limit"));
            body = body.add(text("Daemon not reachable."));
            if let Some(error) = &self.error {
                body = body.add(text(error.clone()));
            }
            body = body.add(divider::horizontal::default());
            body = body.add(text("Start it with:"));
            body = body.add(text("sudo systemctl start hp-charge-limit"));
            return column![body].width(Length::Fill).into();
        };

        let capacity = status
            .capacity
            .map(|c| format!("{c}%"))
            .unwrap_or_else(|| "--".into());

        let header = row![
            column![text(capacity).size(28), text(status.status.clone())].spacing(2),
            column![
                text(if status.ac { "AC online" } else { "On battery" }),
                text(format!("firmware: {}", status.mode_name)),
            ]
            .spacing(2),
        ]
        .spacing(16)
        .align_y(cosmic::iced::Alignment::Center);

        let mut body = list_column().list_item_padding(8);
        body = body.add(header);
        body = body.add(divider::horizontal::default());

        body = body.add(settings::item(
            "Charge limit",
            toggler(status.enabled).on_toggle(Message::ToggleLimit),
        ));

        let stop_min = (status.start + 1).min(100) as f64;
        let stop_value = (status.stop as f64).clamp(stop_min, 100.0);
        body = body.add(settings::item(
            "Stop at",
            text(format!("{}%", status.stop)),
        ));
        body = body.add(
            slider(stop_min..=100.0, stop_value, Message::StopChanged)
                .step(1.0)
                .width(Length::Fill)
                .on_release(Message::Persist),
        );

        let start_max = status.stop.saturating_sub(1) as f64;
        let start_value = (status.start as f64).clamp(0.0, start_max.max(0.0));
        body = body.add(settings::item(
            "Resume at",
            text(format!("{}%", status.start)),
        ));
        body = body.add(
            slider(0.0..=start_max.max(0.0), start_value, Message::StartChanged)
                .step(1.0)
                .width(Length::Fill)
                .on_release(Message::Persist),
        );

        body = body.add(divider::horizontal::default());
        body = body.add(settings::item("Firmware", text(status.mode_name.clone())));
        body = body.add(settings::item(
            "Poll every",
            text(format!("{} s", status.interval)),
        ));

        if let Some(error) = &self.error {
            body = body.add(settings::item("Error", text(error.clone())));
        }

        column![body].width(Length::Fill).into()
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
