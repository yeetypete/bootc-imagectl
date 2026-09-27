//! Check the installed Arch system.

use anyhow::Result;

use crate::status;

#[test]
fn updates_from_target_imgref() -> Result<()> {
    status::updates_from("localhost/bootc-imagectl-test:arch")
}
