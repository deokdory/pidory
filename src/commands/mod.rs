pub mod branch;
pub mod model;
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
        session::status(),
        session::stop(),
        session::kick(),
        session::sleep(),
        session::clear(),
        session::new(),
        skill::skill(),
        model::model(),
        recall::recall(),
        branch::branch(),
    ]
}
