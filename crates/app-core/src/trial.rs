//! Trying new network values and returning to the old ones (plan 006, decisions N5 and N7).
//!
//! The stored values change only when the candidate is proven, so a reset or a power loss during
//! the trial ends on the previous values. [`Trial`] follows one attempt, [`Stored`] holds the
//! values. Both are pure: the caller passes the time and reports what happened.

use crate::network::NetworkConfig;

/// How long a candidate has to prove itself.
pub const PROOF_TIMEOUT_MS: u64 = 45_000;

/// How far the candidate has to get before it counts as working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proof {
    /// A management session with Home Assistant is open over the new values. This is the full
    /// proof, used for the interface that carries the session now.
    Session,
    /// The interface joined, has an address and reaches Home Assistant over TCP. A weaker
    /// proof for the interface that does not carry the session (N7).
    Link,
}

impl Proof {
    /// The word in `network_result`.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Link => "link",
        }
    }

    /// The proof for a word of `network_result`.
    #[must_use]
    pub fn from_wire(word: &str) -> Option<Self> {
        match word {
            "session" => Some(Self::Session),
            "link" => Some(Self::Link),
            _ => None,
        }
    }
}

/// Why a candidate was given up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The tablet could not join the Wi-Fi network (name, password or reach).
    NoAssociation,
    /// Joined, but no address arrived or the static address could not be used.
    NoAddress,
    /// An address, but no path to Home Assistant (a wrong router or DNS server joins fine).
    NoHomeAssistant,
    /// The message was malformed, expired, stale or invalid, so nothing was tried.
    RejectedValues,
}

impl Reason {
    /// The word in `network_result`.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::NoAssociation => "no_association",
            Self::NoAddress => "no_address",
            Self::NoHomeAssistant => "no_home_assistant",
            Self::RejectedValues => "rejected_values",
        }
    }

    /// The reason for a word of `network_result`.
    #[must_use]
    pub fn from_wire(word: &str) -> Option<Self> {
        match word {
            "no_association" => Some(Self::NoAssociation),
            "no_address" => Some(Self::NoAddress),
            "no_home_assistant" => Some(Self::NoHomeAssistant),
            "rejected_values" => Some(Self::RejectedValues),
            _ => None,
        }
    }
}

/// What the caller does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Keep waiting.
    Continue,
    /// The candidate works: store it and report `applied` with this proof.
    Commit(Proof),
    /// Return to the previous values and report `reverted` with this reason.
    Revert(Reason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Joining,
    WaitingForAddress,
    WaitingForHomeAssistant,
}

/// One attempt with a new configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trial {
    proof: Proof,
    stage: Stage,
    deadline_ms: u64,
    finished: bool,
}

impl Trial {
    /// Starts an attempt at `now_ms`.
    #[must_use]
    pub const fn start(now_ms: u64, proof: Proof) -> Self {
        Self {
            proof,
            stage: Stage::Joining,
            deadline_ms: now_ms.saturating_add(PROOF_TIMEOUT_MS),
            finished: false,
        }
    }

    const fn settle(&mut self, step: Step) -> Step {
        if self.finished {
            return Step::Continue;
        }
        if !matches!(step, Step::Continue) {
            self.finished = true;
        }
        step
    }

    /// The interface joined the network (for a wired one: the link came up).
    pub fn joined(&mut self) -> Step {
        if self.stage == Stage::Joining {
            self.stage = Stage::WaitingForAddress;
        }
        self.settle(Step::Continue)
    }

    /// The interface has an address.
    pub const fn address(&mut self) -> Step {
        self.stage = Stage::WaitingForHomeAssistant;
        self.settle(Step::Continue)
    }

    /// The session with Home Assistant is open over the new values.
    pub fn session_opened(&mut self) -> Step {
        if self.stage != Stage::WaitingForHomeAssistant || self.proof != Proof::Session {
            return Step::Continue;
        }
        self.settle(Step::Commit(Proof::Session))
    }

    /// A TCP connection to Home Assistant over the interface succeeded.
    pub fn reached(&mut self) -> Step {
        if self.stage != Stage::WaitingForHomeAssistant || self.proof != Proof::Link {
            return Step::Continue;
        }
        self.settle(Step::Commit(Proof::Link))
    }

    /// The driver said the attempt cannot succeed (wrong password, no such network).
    pub const fn failed(&mut self, reason: Reason) -> Step {
        self.settle(Step::Revert(reason))
    }

    /// Time passes. At the deadline the reason is the stage the attempt reached.
    pub const fn tick(&mut self, now_ms: u64) -> Step {
        if now_ms < self.deadline_ms {
            return Step::Continue;
        }
        let reason = match self.stage {
            Stage::Joining => Reason::NoAssociation,
            Stage::WaitingForAddress => Reason::NoAddress,
            Stage::WaitingForHomeAssistant => Reason::NoHomeAssistant,
        };
        self.settle(Step::Revert(reason))
    }
}

/// The values the tablet keeps, and the candidate it is trying.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stored {
    current: NetworkConfig,
    candidate: Option<NetworkConfig>,
}

impl Stored {
    #[must_use]
    pub const fn new(current: NetworkConfig) -> Self {
        Self {
            current,
            candidate: None,
        }
    }

    /// What to write to storage: the candidate never is, until [`commit`](Self::commit).
    #[must_use]
    pub const fn persisted(&self) -> &NetworkConfig {
        &self.current
    }

    /// What the interfaces should use now: the candidate while one is tried.
    #[must_use]
    pub fn in_use(&self) -> &NetworkConfig {
        self.candidate.as_ref().unwrap_or(&self.current)
    }

    /// Whether a candidate is being tried.
    #[must_use]
    pub const fn trying(&self) -> bool {
        self.candidate.is_some()
    }

    /// Starts trying `candidate`. An interface the candidate does not mention keeps its values.
    pub fn begin(&mut self, candidate: &NetworkConfig) {
        self.candidate = Some(NetworkConfig {
            wifi: candidate.wifi.clone().or_else(|| self.current.wifi.clone()),
            ethernet: candidate
                .ethernet
                .clone()
                .or_else(|| self.current.ethernet.clone()),
        });
    }

    /// The candidate worked: it becomes the stored value. Returns it for writing to storage.
    pub fn commit(&mut self) -> &NetworkConfig {
        if let Some(candidate) = self.candidate.take() {
            self.current = candidate;
        }
        &self.current
    }

    /// The candidate failed, or the tablet restarted: the previous values stay.
    pub fn abandon(&mut self) {
        self.candidate = None;
    }
}
