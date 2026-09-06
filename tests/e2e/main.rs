//! Run bootc-imagectl inside a container and check the result. The tests
//! run in a container of each image built from the Containerfiles here.
//! run.sh starts those containers (see the justfile). A module named after an
//! image contains the tests specific to that image.

mod arch;
mod finalize;
