use crate::app::App;

mod app;
mod socket;

fn main() -> cosmic::iced::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    cosmic::applet::run::<App>(())
}
