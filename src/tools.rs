use crate::state::State;

pub mod classify;
pub mod hits;
pub mod listing;
pub mod resolution;

mcplease::tools!(
    State,
    (Show, show, "show"),
    (Uses, uses, "uses"),
    (Action, action, "action")
);
