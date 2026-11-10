//! The simulator's scripted firmware install (decision D7).
//!
//! The simulator runs the real checks of `njorun_app_core::install` on every `install`
//! message and then plays a scripted flow instead of downloading and writing an image. The
//! script comes from the pre-release part of the requested version, so a qualification run
//! picks the outcome with the version it asks HA to install:
//!
//! | version | flow |
//! | --- | --- |
//! | `9.0.0` | accept, progress through all stages, report `installed` |
//! | `9.0.0-rollback` | the same, but the new boot is not confirmed: `rolled_back` |
//! | `9.0.0-fail` | accept, stop after the first verify step: `rolled_back` |
//! | `9.0.0-reject.<reason>` | the real checks pass, then the answer is `rejected` with `<reason>` |
//!
//! `reject.<reason>` lets a run exercise HA's handling of reasons that HA itself never
//! provokes (`expired`, `duplicate`, `busy`, `malformed`); the real checks still answer for
//! `incompatible` (a version below 0.1.0) and `too_large` (a size above 8 MiB).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use njorun_app_core::install::{
    Context, Install, InstallGate, Outcome, ProgressPacer, Reject, Stage, outcome_message,
    progress_message, result_message,
};
use njorun_ha_client::{ClientError, Connection, ManagementSession};
use serde_json::Value;

/// The firmware floor of the simulator: it refuses to be replaced by anything older.
const FLOOR: (u64, u64, u64) = (0, 1, 0);
/// Time between two scripted steps. Above the pacing minimum so every step is sent.
const STEP: Duration = Duration::from_millis(1_100);

/// What the version asks the simulator to do after the checks passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Install,
    Rollback,
    Fail,
    Reject(Reject),
}

/// Reads the script from the pre-release part of a version.
#[must_use]
pub fn script(version: &str) -> Script {
    let build_free = version.split('+').next().unwrap_or(version);
    let Some((_, pre)) = build_free.split_once('-') else {
        return Script::Install;
    };
    match pre {
        "rollback" => Script::Rollback,
        "fail" => Script::Fail,
        _ => pre
            .strip_prefix("reject.")
            .and_then(|reason| {
                [
                    Reject::Expired,
                    Reject::Duplicate,
                    Reject::Busy,
                    Reject::Incompatible,
                    Reject::TooLarge,
                    Reject::Malformed,
                ]
                .into_iter()
                .find(|known| known.as_str() == reason)
            })
            .map_or(Script::Install, Script::Reject),
    }
}

struct Flow {
    install: Install,
    script: Script,
    steps: VecDeque<(Stage, u32)>,
    next_at: Instant,
}

/// The install state of one management session.
pub struct Ota {
    origin: String,
    gate: InstallGate,
    pacer: ProgressPacer,
    flow: Option<Flow>,
    started: Instant,
    /// The version the simulator reports; it changes after an `installed` outcome.
    pub version: String,
}

/// `scheme://authority` of an HA URL.
fn origin_of(url: &str) -> String {
    let scheme_end = url.find("://").map_or(0, |at| at + 3);
    let end = url[scheme_end..]
        .find('/')
        .map_or(url.len(), |at| scheme_end + at);
    url[..end].to_owned()
}

fn steps(size: u32, script: Script) -> VecDeque<(Stage, u32)> {
    let mut all = VecDeque::from([
        (Stage::Download, 0),
        (Stage::Download, size / 2),
        (Stage::Download, size),
        (Stage::Verify, 0),
    ]);
    if script != Script::Fail {
        all.extend([
            (Stage::Verify, size),
            (Stage::Write, 0),
            (Stage::Write, size / 2),
            (Stage::Write, size),
            (Stage::Reboot, size),
        ]);
    }
    all
}

impl Ota {
    #[must_use]
    pub fn new(ha_url: &str) -> Self {
        Self {
            origin: origin_of(ha_url),
            gate: InstallGate::new(),
            pacer: ProgressPacer::new(),
            flow: None,
            started: Instant::now(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    /// Checks one decrypted `install` and sends its `install_result`.
    ///
    /// # Errors
    /// Transport errors of the session.
    pub fn install(
        &mut self,
        message: &Value,
        now: u64,
        connection: &mut Connection,
        session: &mut ManagementSession,
    ) -> Result<(), ClientError> {
        let context = Context {
            now,
            offered: session.ota,
            ha_origin: &self.origin,
            floor: Some(FLOOR),
        };
        let answer = match self.gate.accept(message, &context) {
            Ok(install) => match script(&install.version) {
                Script::Reject(reason) => {
                    self.gate.finish();
                    Some(result_message(&install.install_id, Err(reason)))
                }
                script => {
                    let answer = result_message(&install.install_id, Ok(()));
                    self.flow = Some(Flow {
                        steps: steps(install.size, script),
                        install,
                        script,
                        next_at: Instant::now() + STEP,
                    });
                    Some(answer)
                }
            },
            Err(refusal) => refusal.result(),
        };
        if let Some(answer) = answer {
            session.send(connection, &answer)?;
        }
        Ok(())
    }

    /// Plays the next scripted step when it is due.
    ///
    /// # Errors
    /// Transport errors of the session.
    pub fn tick(
        &mut self,
        connection: &mut Connection,
        session: &mut ManagementSession,
    ) -> Result<(), ClientError> {
        let Some(flow) = self.flow.as_mut() else {
            return Ok(());
        };
        if Instant::now() < flow.next_at {
            return Ok(());
        }
        if let Some(&(stage, done)) = flow.steps.front() {
            let total = flow.install.size;
            let now_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if self.pacer.due(now_ms, stage, done, total) {
                flow.steps.pop_front();
                let message = progress_message(&flow.install.install_id, stage, done, total)
                    .ok_or(ClientError::Protocol)?;
                session.send(connection, &message)?;
            }
            flow.next_at = Instant::now() + STEP;
            if !flow.steps.is_empty() {
                return Ok(());
            }
        }
        // The script is over: reconcile as the tablet does after its reboot.
        let flow = self.flow.take().ok_or(ClientError::Protocol)?;
        let confirmed = flow.script == Script::Install;
        let running = if flow.script == Script::Install {
            flow.install.version.as_str()
        } else {
            self.version.as_str()
        };
        let outcome = Outcome::reconcile(&flow.install.version, running, confirmed);
        let reported = if outcome == Outcome::Installed {
            flow.install.version.clone()
        } else {
            self.version.clone()
        };
        let message = outcome_message(&flow.install.install_id, outcome, &reported)
            .ok_or(ClientError::Protocol)?;
        session.send(connection, &message)?;
        self.version = reported;
        self.gate.finish();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use njorun_app_core::install::{Reject, Stage};

    use super::{Script, origin_of, script, steps};

    #[test]
    fn the_version_picks_the_script() {
        assert_eq!(script("9.0.0"), Script::Install);
        assert_eq!(script("9.0.0-beta.1"), Script::Install);
        assert_eq!(script("9.0.0-rollback+abc"), Script::Rollback);
        assert_eq!(script("9.0.0-fail"), Script::Fail);
        assert_eq!(script("9.0.0-reject.busy"), Script::Reject(Reject::Busy));
        assert_eq!(
            script("9.0.0-reject.too_large"),
            Script::Reject(Reject::TooLarge)
        );
        assert_eq!(script("9.0.0-reject.whatever"), Script::Install);
    }

    #[test]
    fn origins_drop_the_path() {
        assert_eq!(origin_of("http://127.0.0.1:8123"), "http://127.0.0.1:8123");
        assert_eq!(origin_of("https://ha.local:8123/"), "https://ha.local:8123");
        assert_eq!(origin_of("https://ha.local/x/y"), "https://ha.local");
    }

    #[test]
    fn a_failing_flow_stops_after_the_first_verify_step() {
        let failing = steps(100, Script::Fail);
        assert_eq!(failing.back(), Some(&(Stage::Verify, 0)));
        let full = steps(100, Script::Install);
        assert_eq!(full.back(), Some(&(Stage::Reboot, 100)));
        assert!(
            full.iter()
                .zip(full.iter().skip(1))
                .all(|(a, b)| a.0 <= b.0)
        );
    }
}
