pub mod recall;
pub mod register;
pub mod session;
pub mod skill;

use crate::{Data, Error};

pub fn all_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        register::register(),
        register::unregister(),
        register::new_project(),
        session::sessions(),
        session::list(),
        session::del(),
        session::status(),
        session::stop(),
        skill::skill(),
        recall::recall(),
    ]
}
