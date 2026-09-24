//! Pausing: by hand, or while connected to one of the chosen networks.
//!
//! Networks are NetworkManager connections, by the name the network applet
//! shows. Asked through `nmcli`, which comes with NetworkManager; without it
//! there are no networks to pick and only the manual pause applies.

use crate::i18n::{fill, t};

/// Why syncing is paused right now, worded for the tray and the window, or
/// `None` when it is not.
pub fn reason() -> Option<String> {
    let config = crate::config::load();
    if config.sync_paused {
        return Some(t("Paused").into());
    }
    if config.pause_on_networks.is_empty() {
        return None;
    }
    let active = active_connections();
    let network = config.pause_on_networks.iter().find(|n| active.contains(n))?;
    Some(fill(t("Paused while connected to {network}"), &[("network", network)]))
}

/// Pauses or resumes by hand, and tells a running daemon at once.
pub fn set(paused: bool) -> anyhow::Result<()> {
    let mut config = crate::config::load();
    config.sync_paused = paused;
    crate::config::save(&config)?;
    crate::daemon::poke();
    Ok(())
}

/// The connections that are up, by name.
pub fn active_connections() -> Vec<String> {
    nmcli(&["-f", "NAME", "connection", "show", "--active"]).lines().map(str::to_owned).collect()
}

/// The connections worth offering, sorted: Wi-Fi, wired, mobile and VPN.
pub fn known_connections() -> Vec<String> {
    user_connections(&nmcli(&["-f", "NAME,TYPE", "connection", "show"]))
}

/// Bridges, loopback and the like are plumbing, not networks anyone joins.
fn user_connections(listing: &str) -> Vec<String> {
    const KINDS: [&str; 7] = ["802-11-wireless", "802-3-ethernet", "gsm", "cdma", "bluetooth", "vpn", "wireguard"];
    // The type never contains a colon; a name may.
    let mut names: Vec<String> = listing
        .lines()
        .filter_map(|l| l.rsplit_once(':'))
        .filter(|(_, kind)| KINDS.contains(kind))
        .map(|(name, _)| name.to_owned())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Terse, unescaped output: one connection per line, names verbatim, trailing
/// spaces included, as NetworkManager stores them.
fn nmcli(args: &[&str]) -> String {
    std::process::Command::new("nmcli")
        .args(["-t", "-e", "no"])
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_networks_people_join() {
        let listing = "lo:loopback\ndocker0:bridge\nHome:802-11-wireless\nCafé: guest:802-11-wireless\nWired connection 1:802-3-ethernet\nProtonVPN:wireguard\nPhone :802-11-wireless\n";
        assert_eq!(user_connections(listing), ["Café: guest", "Home", "Phone ", "ProtonVPN", "Wired connection 1"]);
    }
}
