//! Whether a newer kpdrive has been released, and installing it from that
//! release's own packages through the distribution's package manager.
//!
//! Only an install that came from those packages is updated this way: a build
//! from source, or a distribution there are no packages for, is told about the
//! release and pointed at its page.

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

const LATEST: &str = "https://api.github.com/repos/creadri/kpdrive/releases/latest";
const USER_AGENT: &str = concat!("kpdrive/", env!("CARGO_PKG_VERSION"));
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Every package a release ships for a system. Only those installed here are
/// updated: someone without Dolphin has no use for its plugin.
const PACKAGES: [&str; 3] = ["kpdrive", "kpdrive-ui", "kpdrive-dolphin"];

/// The release asset listing each package's SHA-256, as `sha256sum` writes it.
const SUMS: &str = "SHA256SUMS";

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The release's page on GitHub, for whoever installs by hand.
    pub page: String,
    pub assets: Vec<Asset>,
}

impl Release {
    fn parse(v: &serde_json::Value) -> Option<Self> {
        let tag = v["tag_name"].as_str()?;
        Some(Self {
            version: tag.strip_prefix('v').unwrap_or(tag).to_owned(),
            page: v["html_url"].as_str().unwrap_or_default().to_owned(),
            assets: v["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| Some(Asset { name: a["name"].as_str()?.to_owned(), url: a["browser_download_url"].as_str()?.to_owned() }))
                .collect(),
        })
    }
}

/// How packages are installed here.
#[derive(Clone, Debug, PartialEq)]
pub enum System {
    /// Fedora: `dist` is the tag in the file name, such as `fc44`.
    Rpm { dist: String, arch: String },
    Deb { arch: String },
}

/// The files that bring this system up to a release.
#[derive(Clone, Debug)]
pub struct Plan {
    pub system: System,
    pub files: Vec<Asset>,
    sums: Asset,
}

/// A release newer than this one. Without a plan it cannot be installed from
/// here, only downloaded from its page.
#[derive(Clone, Debug)]
pub struct Available {
    pub release: Release,
    pub plan: Option<Plan>,
}

pub enum Outcome {
    Installed,
    /// The password prompt was dismissed. Nothing changed, and nothing failed.
    Declined,
}

/// Whether `candidate` is a later version than `current`. Parts are compared
/// as numbers, so 0.10.0 comes after 0.9.0; a pre-release suffix is dropped,
/// as the releases asked about are never pre-releases.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('-').next().unwrap_or_default().split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    parts(candidate) > parts(current)
}

/// The latest release, when it is newer than this build.
pub async fn check() -> Result<Option<Available>> {
    let http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
    let v: serde_json::Value = http
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("ask GitHub for the latest release")?
        .error_for_status()?
        .json()
        .await?;
    let release = Release::parse(&v).ok_or_else(|| anyhow!("GitHub's answer names no release"))?;
    if !newer(&release.version, CURRENT) {
        return Ok(None);
    }
    let plan = detect().and_then(|(system, packages)| plan_for(&release, &system, &packages));
    Ok(Some(Available { release, plan }))
}

/// The distribution family, from the text of `/etc/os-release`.
#[derive(Debug, PartialEq)]
enum Family {
    Fedora(String),
    Debian,
}

fn family(os_release: &str) -> Option<Family> {
    let fields: HashMap<&str, &str> = os_release
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim().trim_matches(|c| c == '"' || c == '\'')))
        .collect();
    // Only the distributions packages are built for: a derivative's libraries
    // need not match, and a failed install halfway is worse than none.
    match fields.get("ID")? {
        &"fedora" => Some(Family::Fedora(fields.get("VERSION_ID")?.to_string())),
        &"debian" => Some(Family::Debian),
        _ => None,
    }
}

/// How this machine installs packages, and which of ours it has. `None` when
/// kpdrive did not come from a package here.
fn detect() -> Option<(System, Vec<String>)> {
    let text = std::fs::read_to_string("/etc/os-release").or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).ok()?;
    let family = family(&text)?;
    // The architecture of an installed package, or `None` when it is not installed.
    let installed = |package: &str| -> Option<String> {
        let out = match family {
            Family::Fedora(_) => std::process::Command::new("rpm").args(["-q", "--qf", "%{ARCH}", package]).output(),
            Family::Debian => {
                std::process::Command::new("dpkg-query").args(["-W", "-f=${db:Status-Status} ${Architecture}", package]).output()
            }
        }
        .ok()
        .filter(|o| o.status.success())?;
        let text = String::from_utf8(out.stdout).ok()?;
        match family {
            Family::Fedora(_) => Some(text.trim().to_owned()),
            // A removed package whose settings remain is still listed.
            Family::Debian => text.strip_prefix("installed ").map(|a| a.trim().to_owned()),
        }
    };
    let arch = installed(PACKAGES[0])?;
    let packages = PACKAGES.iter().filter(|p| installed(p).is_some()).map(|p| p.to_string()).collect();
    let system = match family {
        Family::Fedora(version) => System::Rpm { dist: format!("fc{version}"), arch },
        Family::Debian => System::Deb { arch },
    };
    Some((system, packages))
}

/// The release's file for each installed package, or `None` when one is
/// missing, or there is nothing to check them against.
fn plan_for(release: &Release, system: &System, packages: &[String]) -> Option<Plan> {
    let version = &release.version;
    let matches = |package: &str, name: &str| match system {
        // kpdrive-0.6.0-1.fc44.x86_64.rpm, whatever the release number.
        System::Rpm { dist, arch } => name
            .strip_prefix(&format!("{package}-{version}-"))
            .and_then(|rest| rest.strip_suffix(&format!(".{dist}.{arch}.rpm")))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())),
        System::Deb { arch } => name == format!("{package}_{version}_{arch}.deb"),
    };
    let files = packages
        .iter()
        .map(|p| release.assets.iter().find(|a| matches(p, &a.name)).cloned())
        .collect::<Option<Vec<_>>>()?;
    let sums = release.assets.iter().find(|a| a.name == SUMS)?.clone();
    Some(Plan { system: system.clone(), files, sums })
}

/// File name → SHA-256, from `sha256sum` output. A `*` before the name marks
/// binary mode, which changes nothing here.
fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .map(|(hash, name)| (name.trim_start().trim_start_matches('*').to_owned(), hash.to_ascii_lowercase()))
        .collect()
}

fn verify(name: &str, bytes: &[u8], sums: &HashMap<String, String>) -> Result<()> {
    let expected = sums.get(name).ok_or_else(|| anyhow!("{SUMS} does not list {name}"))?;
    let actual = hex::encode(Sha256::digest(bytes));
    if &actual != expected {
        bail!("{name} does not match its checksum; it was not installed");
    }
    Ok(())
}

/// Where downloads wait for the package manager.
fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cache"))
        .join("kpdrive")
}

/// True the first time `version` is asked about, so each release is
/// announced once rather than at every start.
pub fn first_notice(version: &str) -> bool {
    let file = cache_dir().join("update-notified");
    if std::fs::read_to_string(&file).is_ok_and(|v| v.trim() == version) {
        return false;
    }
    let _ = std::fs::create_dir_all(cache_dir());
    let _ = std::fs::write(&file, version);
    true
}

/// Downloads and checks every file, then hands them to the package manager
/// behind a password prompt. Nothing is installed unless every file matches.
pub async fn install(available: &Available) -> Result<Outcome> {
    let plan = available.plan.as_ref().ok_or_else(|| anyhow!("there is no package of this release for this system"))?;
    let http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
    let fetch = |asset: &Asset| {
        let request = http.get(&asset.url);
        let name = asset.name.clone();
        async move {
            let got = async { request.send().await?.error_for_status()?.bytes().await }.await;
            got.with_context(|| format!("download {name}"))
        }
    };
    let sums = parse_sums(&String::from_utf8_lossy(&fetch(&plan.sums).await?));
    let dir = cache_dir().join("updates").join(&available.release.version);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let mut paths = Vec::new();
    for asset in &plan.files {
        let bytes = fetch(asset).await?;
        verify(&asset.name, &bytes, &sums)?;
        let path = dir.join(&asset.name);
        std::fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))?;
        paths.push(path);
    }
    let tool = match plan.system {
        System::Rpm { .. } => "dnf",
        System::Deb { .. } => "apt-get",
    };
    let out = tokio::process::Command::new("pkexec")
        .arg(tool)
        .args(["install", "-y"])
        .args(&paths)
        .output()
        .await
        .context("run pkexec, which comes with polkit")?;
    let _ = std::fs::remove_dir_all(&dir);
    match out.status.code() {
        Some(0) => Ok(Outcome::Installed),
        // pkexec's own: the prompt was dismissed.
        Some(126) => Ok(Outcome::Declined),
        Some(127) => bail!("not authorised to install packages"),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given");
            bail!("{tool} failed: {why}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What v0.6.0 actually shipped.
    fn release(version: &str, sums: bool) -> Release {
        let mut names = vec![
            "kpdrive-0.6.0-1.fc43.src.rpm",
            "kpdrive-0.6.0-1.fc43.x86_64.rpm",
            "kpdrive-0.6.0-1.fc44.src.rpm",
            "kpdrive-0.6.0-1.fc44.x86_64.rpm",
            "kpdrive-0.6.0-vendor.tar.xz",
            "kpdrive-dbgsym_0.6.0_amd64.deb",
            "kpdrive-dolphin-0.6.0-1.fc43.x86_64.rpm",
            "kpdrive-dolphin-0.6.0-1.fc44.x86_64.rpm",
            "kpdrive-dolphin-dbgsym_0.6.0_amd64.deb",
            "kpdrive-dolphin_0.6.0_amd64.deb",
            "kpdrive-ui-0.6.0-1.fc43.x86_64.rpm",
            "kpdrive-ui-0.6.0-1.fc44.x86_64.rpm",
            "kpdrive-ui-dbgsym_0.6.0_amd64.deb",
            "kpdrive-ui_0.6.0_amd64.deb",
            "kpdrive_0.6.0_amd64.deb",
        ];
        if sums {
            names.push(SUMS);
        }
        let assets = names.into_iter().map(|n| Asset { name: n.into(), url: format!("https://example/{n}") }).collect();
        Release { version: version.into(), page: String::new(), assets }
    }

    fn names(plan: &Plan) -> Vec<&str> {
        plan.files.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("0.6.1", "0.6.0"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.6.0", "0.6.0"));
        assert!(!newer("0.5.1", "0.6.0"));
        assert!(!newer("0.6.0-rc1", "0.6.0"), "a pre-release is not newer than its release");
    }

    #[test]
    fn the_release_is_read_from_githubs_answer() {
        let v = serde_json::json!({
            "tag_name": "v0.7.0",
            "html_url": "https://github.com/creadri/kpdrive/releases/tag/v0.7.0",
            "assets": [{ "name": "SHA256SUMS", "browser_download_url": "https://dl/SHA256SUMS" }],
        });
        let r = Release::parse(&v).unwrap();
        assert_eq!(r.version, "0.7.0");
        assert_eq!(r.assets, vec![Asset { name: SUMS.into(), url: "https://dl/SHA256SUMS".into() }]);
    }

    #[test]
    fn only_the_distributions_packages_are_built_for() {
        let fedora = "NAME=\"Fedora Linux\"\nVERSION_ID=44\nID=fedora\n";
        assert_eq!(family(fedora), Some(Family::Fedora("44".into())));
        let debian = "PRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nVERSION_ID=\"13\"\nID=debian\n";
        assert_eq!(family(debian), Some(Family::Debian));
        let ubuntu = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n";
        assert_eq!(family(ubuntu), None, "a derivative's libraries need not match");
    }

    #[test]
    fn picks_the_installed_packages_for_this_system() {
        let fc44 = System::Rpm { dist: "fc44".into(), arch: "x86_64".into() };
        let all: Vec<String> = PACKAGES.iter().map(|p| p.to_string()).collect();
        let plan = plan_for(&release("0.6.0", true), &fc44, &all).unwrap();
        assert_eq!(names(&plan), ["kpdrive-0.6.0-1.fc44.x86_64.rpm", "kpdrive-ui-0.6.0-1.fc44.x86_64.rpm", "kpdrive-dolphin-0.6.0-1.fc44.x86_64.rpm"]);

        let some = vec!["kpdrive".to_string(), "kpdrive-ui".to_string()];
        let deb = System::Deb { arch: "amd64".into() };
        let plan = plan_for(&release("0.6.0", true), &deb, &some).unwrap();
        assert_eq!(names(&plan), ["kpdrive_0.6.0_amd64.deb", "kpdrive-ui_0.6.0_amd64.deb"], "never debug symbols");

        let fc42 = System::Rpm { dist: "fc42".into(), arch: "x86_64".into() };
        assert!(plan_for(&release("0.6.0", true), &fc42, &all).is_none(), "no packages for that Fedora");
        assert!(plan_for(&release("0.6.0", false), &fc44, &all).is_none(), "nothing to check them against");
    }

    #[test]
    fn files_are_checked_against_the_sums() {
        let good = hex::encode(Sha256::digest(b"package"));
        let sums = parse_sums(&format!("{good}  kpdrive_0.7.0_amd64.deb\n{good} *kpdrive-ui_0.7.0_amd64.deb\n"));
        assert!(verify("kpdrive_0.7.0_amd64.deb", b"package", &sums).is_ok());
        assert!(verify("kpdrive-ui_0.7.0_amd64.deb", b"package", &sums).is_ok(), "binary mode marker");
        assert!(verify("kpdrive_0.7.0_amd64.deb", b"tampered", &sums).is_err());
        assert!(verify("kpdrive-dolphin_0.7.0_amd64.deb", b"package", &sums).is_err(), "unlisted");
    }
}
