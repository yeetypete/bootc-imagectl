//! Check the finalized image boots.

use anyhow::Result;

use crate::run_in_vm;

#[test]
fn boots_to_running() -> Result<()> {
    let output = run_in_vm("systemctl is-system-running")?;
    assert!(output.contains("running"), "{output}");
    Ok(())
}
