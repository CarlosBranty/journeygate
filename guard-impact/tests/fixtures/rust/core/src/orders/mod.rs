use crate::money::fmt;

mod line;

use self::line::Line;

pub struct Order(pub Vec<Line>);

impl Order {
    pub fn label(&self) -> String {
        fmt(self.0.len() as u64)
    }
}
