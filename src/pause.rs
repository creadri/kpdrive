//! Pausing: by hand, while connected to one of the chosen networks, or while
//! the machine runs on a chosen power source.
//!
//! Networks are NetworkManager connections, by the name the network applet
//! shows. Asked through `nmcli`, which comes with NetworkManager; without it
//! there are no networks to pick and only the manual pause applies.
//!
//! The power source is the profile Plasma's power management applies (AC,
//! battery or low battery), so "low" means what the user set in System
//! Settings. Without PowerDevil it is read from the kernel instead.

use crate::config::Power;
use crate::i18n::{fill, t};

/// Why syncing is paused right now, worded for the tray and the window, or
/// `None` when it is not.
pub fn reason() -> Option<String> {
    let config = crate::config::load();
    if config.sync_paused {
        return Some(t("Paused").into());
    }
    if !config.pause_on_power.is_empty() {
        let power = power();
        if pauses_on(&config.pause_on_power, power) {
            return Some(
                match power {
                    Power::Ac => t("Paused while on AC power"),
                    Power::Battery => t("Paused while on battery"),
                    Power::LowBattery => t("Paused while the battery is low"),
                }
                .into(),
            );
        }
    }
    if config.pause_on_networks.is_empty() {
        return None;
    }
    let active = active_connections();
    let network = config.pause_on_networks.iter().find(|n| active.contains(n))?;
    Some(fill(t("Paused while connected to {network}"), &[("network", network)]))
}

/// Pausing on battery covers a low battery too: resuming just as the charge
/// runs short would be the wrong way round.
pub fn pauses_on(chosen: &[Power], now: Power) -> bool {
    chosen.contains(&now) || (now == Power::LowBattery && chosen.contains(&Power::Battery))
}

/// The power source right now.
pub fn power() -> Power {
    powerdevil_profile().unwrap_or_else(|| kernel_power(std::path::Path::new("/sys/class/power_supply"), low_level()))
}

/// Plasma's own answer, which already applies the user's low-battery level.
fn powerdevil_profile() -> Option<Power> {
    let out = std::process::Command::new("busctl")
        .args(["--user", "call", "org.kde.Solid.PowerManagement", "/org/kde/Solid/PowerManagement"])
        .args(["org.kde.Solid.PowerManagement", "currentProfile"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    // `s "LowBattery"`
    match String::from_utf8_lossy(&out.stdout).trim().strip_prefix("s ")?.trim_matches('"') {
        "AC" => Some(Power::Ac),
        "Battery" => Some(Power::Battery),
        "LowBattery" => Some(Power::LowBattery),
        _ => None,
    }
}

/// The low-battery level set in Plasma's power settings, or its default.
fn low_level() -> u8 {
    // Beside kpdrive's own folder, in the XDG config directory.
    crate::config::path()
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(|dir| std::fs::read_to_string(dir.join("powerdevilrc")).ok())
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("BatteryLowLevel=")?.trim().parse().ok()))
        .unwrap_or(20)
}

/// Mains plugged in, or no system battery at all, is AC. A mouse's or a
/// phone's battery (scope Device) is not the machine's.
fn kernel_power(dir: &std::path::Path, low: u8) -> Power {
    let read = |p: &std::path::Path, f: &str| std::fs::read_to_string(p.join(f)).map(|s| s.trim().to_owned()).unwrap_or_default();
    let mut batteries = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        match read(&p, "type").as_str() {
            "Mains" | "USB" if read(&p, "online") == "1" && read(&p, "scope") != "Device" => return Power::Ac,
            "Battery" if read(&p, "scope") != "Device" => batteries.push(read(&p, "capacity").parse::<u8>().unwrap_or(100)),
            _ => {}
        }
    }
    match batteries.len() {
        0 => Power::Ac,
        n if (batteries.iter().map(|&c| c as usize).sum::<usize>() / n) <= low as usize => Power::LowBattery,
        _ => Power::Battery,
    }
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

    #[test]
    fn battery_covers_low_battery() {
        assert!(pauses_on(&[Power::Battery], Power::LowBattery));
        assert!(!pauses_on(&[Power::LowBattery], Power::Battery));
        assert!(!pauses_on(&[Power::Battery], Power::Ac));
        assert!(pauses_on(&[Power::Ac], Power::Ac));
    }

    #[test]
    fn power_from_the_kernel() {
        let dir = std::env::temp_dir().join(format!("kpdrive-power-{}", std::process::id()));
        let supply = |name: &str, files: &[(&str, &str)]| {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            for (f, v) in files {
                std::fs::write(dir.join(name).join(f), format!("{v}\n")).unwrap();
            }
        };
        supply("AC", &[("type", "Mains"), ("online", "0")]);
        supply("BAT0", &[("type", "Battery"), ("capacity", "15")]);
        supply("hidpp_battery_0", &[("type", "Battery"), ("capacity", "90"), ("scope", "Device")]);
        assert_eq!(kernel_power(&dir, 20), Power::LowBattery);
        assert_eq!(kernel_power(&dir, 10), Power::Battery);
        supply("AC", &[("online", "1")]);
        assert_eq!(kernel_power(&dir, 20), Power::Ac);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(kernel_power(&dir, 20), Power::Ac);
    }
}
