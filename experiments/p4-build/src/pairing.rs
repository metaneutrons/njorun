//! The tablet announces itself to Home Assistant over HTTP (plan 004).
//!
//! There is no sign-in on the tablet: it posts its announcement to the integration's endpoint,
//! shows the comparison code while an administrator approves it in Home Assistant, and then
//! opens the grant that carries the tablet's own Home Assistant access. One call of [`round`]
//! is one announcement; the caller repeats it while the answer is `Waiting`.

use njorun_app_core::problems::ConnectionProblem;
use njorun_management::pairing::{announce_body, display_code, parse_reply, tablet_name, Reply};
use njorun_management::public_key;

use crate::{board, management, store};

const ENDPOINT_PATH: &str = "/api/njorun/announce";

/// What one announcement led to.
pub(crate) enum Round {
    /// Waiting for an administrator: the tablet shows its name and this code.
    Waiting { name: String, code: String },
    /// Approved, and Home Assistant sent the tablet's access. The binding and the session are
    /// stored; the caller starts the session.
    Granted(store::HomeAssistantSession),
    /// Approved, but no grant came with the answer (the first confirmed session of this binding
    /// is over). The caller asks again with `need_grant`.
    ApprovedWithoutGrant,
    /// Another key is paired under this tablet ID.
    Conflict,
    /// The address answers, but the Njörun integration is not installed there.
    NotInstalled,
    /// Home Assistant rejected the request itself.
    Rejected,
    /// Home Assistant is busy: too many requests, or too many tablets waiting.
    Busy,
}

/// Why no answer came.
pub(crate) enum Failure {
    /// The connection to the address failed; the problem names the likely cause.
    Connection(ConnectionProblem),
    /// Home Assistant answered, but not as protocol 1 says, or the tablet has no identity.
    BadAnswer,
}

/// Announces the tablet to the Home Assistant at `endpoint` once.
pub(crate) fn round(endpoint: &str, need_grant: bool) -> Result<Round, Failure> {
    let identity = store::identity().ok_or(Failure::BadAnswer)?;
    let public = public_key(identity.private_key()).map_err(|_| Failure::BadAnswer)?;
    let name = tablet_name(identity.tablet_id());
    let body = announce_body(
        identity.tablet_id(),
        &name,
        &public,
        &management::capabilities(),
        need_grant,
        // What the user chose on the first start; Home Assistant offers it as the starting value.
        store::theme().as_deref(),
    );
    let url = format!("{}{ENDPOINT_PATH}", endpoint.trim_end_matches('/'));
    let (status, answer) = board::post_json(&url, &body).map_err(|code| {
        let problem = ConnectionProblem::from_code(code);
        log::warn!(target: "pair", "announcement failed: {code:#x} ({problem:?})");
        Failure::Connection(problem)
    })?;
    let reply = parse_reply(status, &answer, &public, identity.private_key()).map_err(|error| {
        log::warn!(target: "pair", "unexpected announcement answer (status {status}): {error}");
        Failure::BadAnswer
    })?;
    Ok(match reply {
        Reply::Pending { code } => {
            log::info!(target: "pair", "waiting for approval in Home Assistant, code {code}");
            Round::Waiting {
                name,
                code: display_code(&code),
            }
        }
        Reply::Approved {
            binding_id,
            ha_public,
            grant,
        } => {
            store::save_binding(&binding_id, &ha_public);
            match grant {
                Some(grant) => {
                    log::info!(target: "pair", "approved as binding {binding_id}, access received");
                    let session = store::HomeAssistantSession {
                        url: endpoint.trim_end_matches('/').to_owned(),
                        client_id: grant.client_id,
                        refresh_token: grant.refresh_token,
                    };
                    store::save_home_assistant(&session);
                    Round::Granted(session)
                }
                None => {
                    log::info!(target: "pair", "approved as binding {binding_id}, no access sent");
                    Round::ApprovedWithoutGrant
                }
            }
        }
        Reply::Conflict => {
            log::warn!(target: "pair",
                "another key is paired as {}; remove or replace that tablet in Home Assistant",
                identity.tablet_id()
            );
            Round::Conflict
        }
        Reply::NotInstalled => Round::NotInstalled,
        Reply::Rejected => Round::Rejected,
        Reply::RateLimited | Reply::Busy => Round::Busy,
    })
}
