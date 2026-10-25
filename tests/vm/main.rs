//! Run tests inside the booted image.

use bootc_imagectl::testing;

mod accounts;
mod boot;
mod regular_user;
mod units;

fn main() {
    testing::run(&[
        accounts::TESTS,
        boot::TESTS,
        regular_user::TESTS,
        units::TESTS,
    ]);
}
