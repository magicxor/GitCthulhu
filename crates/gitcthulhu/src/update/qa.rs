//! QA scenario verbs of the update notice (feature `qa`; spec 21 §8.5, "update notice with a fake manifest").
//!
//! A QA run starts no services and never leaves the machine. `update serve` stands up a manifest on the loopback
//! interface and starts the REAL update service against it, so the notice, the settings page and the snooze go
//! through the same code as in a release build:
//!
//! | Verb | Does |
//! |---|---|
//! | `update serve <version> [release-url]` | serves a manifest announcing `<version>`, installs services that use it (the automatic check then runs) |
//! | `update check` | "Check now" of the Updates page (reports in a toast) |
//! | `update snooze <hours>` | "Remind me ..." of the notice's menu |
//! | `assert update notice <version>\|none` | the bottom bar's notice shows `<version>` (or nothing) |
//! | `open update menu` | opens the notice's right-click menu |

use gpui_kit::{App, Global};

use super::loopback::{Loopback, Response, manifest};
use super::{CheckKind, UpdateState, check_now, notice, snapshot, snooze};
use crate::qa::{Verb, register_idle_probe, register_verb};

/// The manifest server of the run (kept alive for the whole process).
struct ManifestServer(#[allow(dead_code)] Loopback);

impl Global for ManifestServer {}

/// Registers the verbs and the idle probe.
pub(super) fn register(cx: &mut App) {
    register_idle_probe(cx, |_, cx| {
        // The startup check is still to come only while automatic checks are on.
        let auto_check = crate::settings::get_or_default(cx).update_check;
        let pending = cx.try_global::<UpdateState>().is_some_and(|s| {
            s.checking
                || s.pending_snoozes > 0
                || (auto_check
                    && s.scheduler_started
                    && s.last_checked.is_none()
                    && s.last_error.is_none())
        });
        pending.then(|| "the update check is running".to_owned())
    });
    register_verb(
        cx,
        Verb::new(
            "update serve",
            "update serve <version> [release-url]",
            "Serves a loopback update manifest announcing <version> and starts the update service against it",
            |_, cx, args| {
                let version = args.text(0, "version")?;
                semver::Version::parse(&version)
                    .map_err(|err| format!("'{version}' is not a version: {err}"))?;
                let release_url = args
                    .opt(1)
                    .unwrap_or_else(|| format!("https://example.com/gitcthulhu/v{version}"));
                let body = manifest(&version, &release_url);
                let server = Loopback::start(move |_| Response::json(body.clone()))
                    .map_err(|err| format!("cannot start the manifest server: {err}"))?;
                let url = server.url("/manifest.json");
                let services = crate::backend::update::services_with_manifest(
                    crate::paths::data_dir(),
                    crate::paths::cache_dir(),
                    Some(&url),
                    "0.0.1",
                )
                .map_err(|err| format!("cannot start the services: {err:#}"))?;
                cx.set_global(ManifestServer(server));
                let Some(state) = crate::app::state_mut(cx) else {
                    return Err("the application state is missing".to_owned());
                };
                state.services = Some(services);
                Ok(format!("manifest at {url}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "update check",
            "update check",
            "Checks for updates like the Check now button of the Updates page",
            |_, cx, _| {
                check_now(cx, CheckKind::Manual);
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "update snooze",
            "update snooze <hours>",
            "Hides the update notice for <hours> (the Remind me entries of its menu use 24 and 168)",
            |_, cx, args| {
                let hours: u32 = args.number(0, "hours")?;
                snooze(cx, hours);
                Ok(format!("{hours} h"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open update menu",
            "open update menu",
            "Opens the right-click menu of the update notice",
            |window, cx, _| {
                if notice(cx).is_none() {
                    return Err("there is no update notice".to_owned());
                }
                super::notice::open_menu(window, cx);
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "assert update notice",
            "assert update notice <version>|none",
            "Fails unless the update notice announces <version> (or there is none)",
            |_, cx, args| {
                let want = args.text(0, "version or none")?;
                let have = snapshot(cx).notice.map(|n| n.version);
                match (want.as_str(), &have) {
                    ("none", None) => Ok("no notice".to_owned()),
                    (version, Some(have)) if version == have => Ok(format!("notice {have}")),
                    (_, have) => Err(format!("expected {want}, the notice is {have:?}")),
                }
            },
        )
        .without_settle(),
    );
}
