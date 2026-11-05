use shop_core::money::fmt;
use std::fmt as stdfmt;

fn main() {
    let _: Option<stdfmt::Error> = None;
    println!("{}", fmt(1));
}
