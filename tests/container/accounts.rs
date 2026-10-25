//! Check the accounts finalize moved to /usr/lib/userdb, for every image.

use anyhow::Result;

use crate::finalize::ROOT;
use bootc_imagectl::testing::Test;
use bootc_imagectl::trial;

/// The user of a removed package keeps its UID and the GID of its group,
/// locked.
fn keeps_removed_accounts_locked() -> Result<()> {
    let user = uzers::get_user_by_name("removed").expect("the user resolves through NSS");
    assert_eq!(user.uid(), 850);
    let group = uzers::get_group_by_gid(850).expect("the group resolves through NSS");
    assert_eq!(group.name(), "removed");
    let record = ROOT.read_to_string("usr/lib/userdb/removed.user")?;
    assert!(record.contains("\"locked\": true"), "{record}");
    Ok(())
}

pub(crate) const TESTS: &[Test] = &[trial!(keeps_removed_accounts_locked)];
