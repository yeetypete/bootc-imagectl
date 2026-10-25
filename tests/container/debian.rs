//! Check `bootc-imagectl finalize` on Debian and Ubuntu.

use std::path::Path;

use anyhow::Result;

use crate::finalize::{ROOT, names, var_tmpfiles};
use bootc_imagectl::testing::{Requirement, Test};
use bootc_imagectl::trial;

fn moves_dpkg_database_and_links_it_back() -> Result<()> {
    assert!(ROOT.exists("usr/lib/sysimage/dpkg/status"));
    assert_eq!(
        ROOT.read_link("var/lib/dpkg")?,
        Path::new("../../usr/lib/sysimage/dpkg")
    );
    assert_eq!(
        ROOT.read_link("var/cache/debconf")?,
        Path::new("../../usr/lib/sysimage/debconf")
    );
    Ok(())
}

fn keeps_apt_log_directory_in_var() -> Result<()> {
    assert!(ROOT.metadata("var/log/apt")?.is_dir());
    Ok(())
}

fn removes_snakeoil_certificate() -> Result<()> {
    assert!(!ROOT.exists("etc/ssl/certs/ssl-cert-snakeoil.pem"));
    assert!(!ROOT.exists("etc/ssl/private/ssl-cert-snakeoil.key"));
    for name in names("etc/ssl/certs")? {
        let target = ROOT.read_link(Path::new("etc/ssl/certs").join(&name)).ok();
        assert_ne!(
            target.as_deref(),
            Some(Path::new("ssl-cert-snakeoil.pem")),
            "{}",
            name.display()
        );
    }
    Ok(())
}

fn keeps_only_expected_entries_in_var() -> Result<()> {
    assert_eq!(names("var")?, ["cache", "lib", "lock", "log", "run", "tmp"]);
    Ok(())
}

fn records_var_entries_in_tmpfiles() -> Result<()> {
    let var = var_tmpfiles()?;
    assert!(
        var.contains("L /var/lib/dpkg - - - - ../../usr/lib/sysimage/dpkg\n"),
        "{var}"
    );
    assert!(var.contains("d /var/mail 2775 root mail -\n"), "{var}");
    Ok(())
}

const REQUIRES: &[Requirement] = &[Requirement::Distro(&["debian", "ubuntu"])];

pub(crate) const TESTS: &[Test] = &[
    trial!(moves_dpkg_database_and_links_it_back, REQUIRES),
    trial!(keeps_apt_log_directory_in_var, REQUIRES),
    trial!(removes_snakeoil_certificate, REQUIRES),
    trial!(keeps_only_expected_entries_in_var, REQUIRES),
    trial!(records_var_entries_in_tmpfiles, REQUIRES),
];
