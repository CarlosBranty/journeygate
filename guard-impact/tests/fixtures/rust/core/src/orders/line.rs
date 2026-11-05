use crate::{
    money::{self, fmt as f},
    orders::Order,
};
use std::collections::HashMap;

pub struct Line;

pub fn describe(_o: &Order) -> String {
    let _ = HashMap::<u8, u8>::new();
    f(1) + &money::fmt(2)
}
