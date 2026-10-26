//! Run tests inside a system `bootc-imagectl install` installed from each
//! image.

use bootc_imagectl::testing;

mod boot;
mod homed;
mod secureboot;
mod status;
mod transient_etc;
mod ubuntu_desktop_example;

fn main() {
    testing::run(&[
        boot::TESTS,
        homed::TESTS,
        secureboot::TESTS,
        status::TESTS,
        transient_etc::TESTS,
        ubuntu_desktop_example::TESTS,
    ]);
}
