pub mod register;
pub mod session;

use crate::{Data, Error};

pub fn all_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        register::register(),
        register::unregister(),
        session::list(),
        session::del(),
        session::status(),
    ]
}
