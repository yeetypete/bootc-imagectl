//! Run tests inside a container of the image.

use bootc_imagectl::testing;

mod accounts;
mod arch;
mod debian;
mod fedora;
mod finalize;
mod homed;
mod regular_user;
mod secureboot;

fn main() {
    testing::run(&[
        accounts::TESTS,
        arch::TESTS,
        debian::TESTS,
        fedora::TESTS,
        finalize::TESTS,
        homed::TESTS,
        regular_user::TESTS,
        secureboot::TESTS,
    ]);
}
