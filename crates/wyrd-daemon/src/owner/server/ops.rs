//! Owner operations: decode parameters, forward through the
//! shared handles, and map failures to wire codes. These are
//! inherent methods on [`super::OwnerServer`]; the parent module
//! keeps the connection loop and dispatch table.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use wyrd_core::policy_channel::{PolicyRequest, PolicyResponse};
use wyrd_core::projection::Projection;
use wyrd_core::read::LiveReader;
use wyrd_core::view::{NamespaceView, Node};

use crate::vault::OwnerView;

use crate::diagnostics::DiagnosticsSetError;
use crate::owner::mapping::{
    map_policy_error, mutation_kind_from_wire, wire_attr, wire_diagnostic_level, wire_dir_entry,
    wire_freshness, wire_identity, wire_mailbox_health, wire_mutation_error, wire_mutation_outcome,
    wire_node, wire_policy_row, wire_sync_report, wire_sync_status,
};
use crate::owner::wire::{
    DiagnosticsSetParams, PathParams, SubmitParams, WireErrorCode, WireLiveness, WireMailboxHealth,
    WireOk, WireParentToken, WirePolicyStatus,
};
use wyrd_settings::DiagnosticLevel;

use super::framing::{
    bound_read_refusal, map_read_error, map_view_error, reject_bound_non_read, submit_content_bytes,
};
use super::socket::{MAX_SUBMIT_CONTENT_BYTES, READ_DEMAND_TIMEOUT};
impl super::OwnerServer {
    /// The current published generation, cloned under a short read
    /// lock. Lock-free after the clone: the generation is immutable,
    /// so a slow consumer pins its own complete snapshot without
    /// blocking publication — including bounded demand waits, which
    /// must never hold the publication lock.
    pub(super) fn current_projection(
        &self,
    ) -> Result<Arc<Projection<OwnerView>>, (WireErrorCode, String)> {
        self.handles
            .projection
            .read()
            .map(|slot| Arc::clone(&slot))
            .map_err(|_| {
                (
                    WireErrorCode::Poisoned,
                    "serving projection lock poisoned".to_string(),
                )
            })
    }

    pub(super) fn op_lookup(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad lookup params: {error}"),
            )
        })?;
        reject_bound_non_read("lookup", &params)?;
        let projection = self.current_projection()?;
        let node = projection
            .view()
            .lookup(&params.path)
            .map_err(map_view_error)?;
        Ok(WireOk::Lookup {
            node: wire_node(&node),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    pub(super) fn op_stat(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad stat params: {error}"),
            )
        })?;
        reject_bound_non_read("stat", &params)?;
        let projection = self.current_projection()?;
        let attr = projection
            .view()
            .stat(&params.path)
            .map_err(map_view_error)?;
        Ok(WireOk::Stat {
            attr: wire_attr(&attr),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    pub(super) fn op_readdir(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad readdir params: {error}"),
            )
        })?;
        reject_bound_non_read("readdir", &params)?;
        let projection = self.current_projection()?;
        let node = projection
            .view()
            .lookup(&params.path)
            .map_err(map_view_error)?;
        // Bound the listing before collecting or mapping it. A lawful
        // tree can hold a million entries; a single owner response is
        // a live operation with its own ceiling. The refused result is
        // empty, so no partial listing reaches the wire.
        let listing = projection
            .view()
            .readdir_budgeted(&node, self.handles.max_readdir_work)
            .map_err(map_view_error)?;
        if listing.limit_exceeded {
            return Err((
                WireErrorCode::TooLarge,
                format!(
                    "directory listing exceeds the interface work bound ({} estimated bytes, max {}); list a smaller subdirectory",
                    listing.work, self.handles.max_readdir_work
                ),
            ));
        }
        Ok(WireOk::Readdir {
            entries: listing.entries.iter().map(wire_dir_entry).collect(),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    pub(super) fn op_read(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad read params: {error}"),
            )
        })?;
        let projection = self.current_projection()?;
        let node = projection
            .view()
            .lookup(&params.path)
            .map_err(map_view_error)?;
        // Identity-bound read: the refusal is decided from the
        // resolved node before any byte is touched — conflicted
        // first, then the token equality — so the check and the
        // served bytes come from one immutable generation.
        // Present must parse — never silently dropped — reusing the
        // node-identity rule; absent stays path-only.
        let expected = wire_identity(&params.identity).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad read identity for {}: {error}", params.path),
            )
        })?;
        if let Some((code, message)) = bound_read_refusal(&node, expected.as_ref(), &params.path) {
            return Err((code, message));
        }
        // Bound one request's memory before touching the bytes: a
        // file past the bound is refused explicitly, never
        // truncated and never streamed past the cap.
        let size = match node {
            Node::File { size, .. } => size,
            // Symlinks resolve at the presentation boundary and
            // conflicts address through version-qualified paths: a
            // non-file never reads, and names the real failure. Same
            // shape as the demand reader's own guard.
            node => match projection.view().open_file(&node) {
                Err(error) => return Err(map_view_error(error)),
                Ok(_) => {
                    return Err((
                        WireErrorCode::NotAFile,
                        format!("{} is not a file", params.path),
                    ));
                }
            },
        };
        if size > self.handles.max_read_bytes {
            return Err((
                WireErrorCode::TooLarge,
                format!(
                    "file is {size} bytes, interface read bound is {}",
                    self.handles.max_read_bytes
                ),
            ));
        }
        let mut reader =
            LiveReader::new(READ_DEMAND_TIMEOUT).with_wants(Arc::clone(&self.handles.wants));
        match (&self.handles.quarantine, &self.handles.scrub) {
            (Some(quarantine), Some(scrub)) => {
                reader = reader.with_repair(Arc::clone(quarantine), Arc::clone(scrub));
            }
            (Some(quarantine), None) => {
                reader = reader.with_quarantine(Arc::clone(quarantine));
            }
            (None, Some(scrub)) => {
                reader = reader.with_scrub(Arc::clone(scrub));
            }
            (None, None) => {}
        }
        let bytes = reader
            .read_file(projection.view(), &params.path)
            .map_err(map_read_error)?;
        Ok(WireOk::Read {
            bytes_hex: hex::encode(&bytes),
            generation: projection.generation(),
            revision: projection.revision(),
        })
    }

    pub(super) fn op_observe(&self) -> Result<WireOk, (WireErrorCode, String)> {
        let slot = self.handles.observation.read().map_err(|_| {
            (
                WireErrorCode::Poisoned,
                "owner observation lock poisoned".to_string(),
            )
        })?;
        match slot.as_ref() {
            Some(observation) => Ok(WireOk::Observe {
                status: wire_sync_status(&observation.status),
            }),
            // Reachable owner, no completed pass: explicit, never a
            // healthy-looking default.
            None => Err((
                WireErrorCode::Unobserved,
                "owner reachable, no sync pass completed yet".to_string(),
            )),
        }
    }

    pub(super) fn op_liveness(&self) -> Result<WireOk, (WireErrorCode, String)> {
        let slot = self.handles.observation.read().map_err(|_| {
            (
                WireErrorCode::Poisoned,
                "owner observation lock poisoned".to_string(),
            )
        })?;
        let paused = self.handles.paused.load(Ordering::Relaxed);
        match slot.as_ref() {
            Some(observation) => Ok(WireOk::Liveness {
                liveness: WireLiveness {
                    health: wire_mailbox_health(&observation.health),
                    report: Some(wire_sync_report(&observation.report)),
                    passes: observation.passes,
                    paused,
                },
            }),
            // No pass yet: unknown health, no report, zero passes.
            // "Owner reachable, no recent sync activity" reads
            // exactly like this — distinct from "owner unreachable",
            // which is a refused connection, not a response, and
            // from paused, which sets the flag below.
            None => Ok(WireOk::Liveness {
                liveness: WireLiveness {
                    health: WireMailboxHealth::unknown(),
                    report: None,
                    passes: 0,
                    paused,
                },
            }),
        }
    }

    /// Set the pause switch: network work and obligation discharge
    /// defer from the next pass; local authoring and serving continue.
    /// Idempotent — pausing twice is one pause — and the answer echoes
    /// the switch as it reads back, so the consumer never infers state
    /// from silence.
    pub(super) fn op_pause(&self) -> Result<WireOk, (WireErrorCode, String)> {
        self.handles.set_paused(true);
        Ok(WireOk::Paused {
            paused: self.handles.paused.load(Ordering::Relaxed),
        })
    }

    /// Clear the pause switch: the next pass is ordinary. Idempotent —
    /// resuming a running drive changes nothing — and the answer echoes
    /// the switch as it reads back.
    pub(super) fn op_resume(&self) -> Result<WireOk, (WireErrorCode, String)> {
        self.handles.set_paused(false);
        Ok(WireOk::Paused {
            paused: self.handles.paused.load(Ordering::Relaxed),
        })
    }

    pub(super) fn op_freshness(&self) -> Result<WireOk, (WireErrorCode, String)> {
        let slot = self.handles.freshness.read().map_err(|_| {
            (
                WireErrorCode::Poisoned,
                "freshness lock poisoned".to_string(),
            )
        })?;
        Ok(WireOk::Freshness {
            freshness: wire_freshness(&slot),
        })
    }

    /// Capture the current parent-directory incarnation for a later
    /// create: the consumer sends the token back with the operation,
    /// and the owner re-checks it before authoring. A path with no
    /// token is minted one here — creates address paths through
    /// tokens, never bare. Refusal means the table is blocked or
    /// saturated, reported as `StaleParent` (retry the capture), never
    /// as a missing parent.
    /// Set the process-local diagnostics level. This is configuration,
    /// not a mutation or a policy operation: it touches neither queue,
    /// commits no drive fact, and changes no store state. The settings
    /// backend owns persistence and the app-group mirror; a mirror
    /// failure is a distinct refusal, never a reported success.
    pub(super) fn op_diagnostics_set(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: DiagnosticsSetParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad diagnostics-set params: {error}"),
            )
        })?;
        let level = match params.level.as_str() {
            "default" => DiagnosticLevel::Default,
            "detailed" => DiagnosticLevel::Detailed,
            other => {
                return Err((
                    WireErrorCode::InvalidDiagnosticLevel,
                    format!(
                        "unknown diagnostics level {other:?}: want \"default\" or \"detailed\""
                    ),
                ))
            }
        };
        let applied = self
            .handles
            .diagnostics
            .set(level)
            .map_err(|error| match error {
                DiagnosticsSetError::Settings(detail) => {
                    (WireErrorCode::DiagnosticsSettings, detail)
                }
                DiagnosticsSetError::Mirror(detail) => (WireErrorCode::DiagnosticsMirror, detail),
                DiagnosticsSetError::Filter(detail) => (WireErrorCode::DiagnosticsFilter, detail),
                other => (WireErrorCode::Unavailable, other.to_string()),
            })?;
        Ok(WireOk::Diagnostics {
            level: wire_diagnostic_level(applied),
        })
    }

    /// Read the applied level back. No parameters, no inference from
    /// silence: the answer is the same value a set operation reports.
    pub(super) fn op_diagnostics_status(&self) -> Result<WireOk, (WireErrorCode, String)> {
        Ok(WireOk::Diagnostics {
            level: wire_diagnostic_level(self.handles.diagnostics.level()),
        })
    }

    pub(super) fn op_capture_parent(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad capture-parent params: {error}"),
            )
        })?;
        reject_bound_non_read("capture-parent", &params)?;
        match self.handles.mutations.capture_parent(&params.path) {
            Ok(Some(token)) => Ok(WireOk::ParentToken {
                token: WireParentToken {
                    instance: self.instance,
                    counter: token.wire_value(),
                },
            }),
            Ok(None) => Err((
                WireErrorCode::StaleParent,
                format!(
                    "parent-token table saturated or path blocked for {:?}: retry the capture",
                    params.path
                ),
            )),
            // A poisoned token table is not a missing parent: name it
            // so the desktop surfaces a fault instead of retrying a
            // stale-parent loop forever.
            Err(error) => {
                let mapped = wire_mutation_error(&error);
                Err((mapped.code, mapped.message))
            }
        }
    }

    /// Forward one mutation to the single queue and block for its
    /// terminal answer. The interface interprets nothing: decode
    /// failures refuse at the wire, queue refusals carry through as
    /// distinct outcomes, and a success means exactly what an
    /// in-process submit's success means. A broken connection while
    /// blocked is unconfirmed, never success and never failure
    /// (child 3 owns that answer). A submit past the content bound
    /// refuses here with `TooLarge` — legibly, with retry guidance —
    /// before any decode or queue admission, so an over-limit submit
    /// never meets the framing silence of an overlong frame.
    pub(super) fn op_submit(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: SubmitParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad submit params: {error}"),
            )
        })?;
        let carried = submit_content_bytes(&params.kind);
        if carried > MAX_SUBMIT_CONTENT_BYTES as u64 {
            return Err((
                WireErrorCode::TooLarge,
                format!(
                    "submit carries {carried} content bytes, over the {} interface submit bound: split into smaller submits or save through the mount",
                    MAX_SUBMIT_CONTENT_BYTES
                ),
            ));
        }
        let kind = mutation_kind_from_wire(&params.kind, self.instance)?;
        match self.handles.mutations.submit(kind) {
            Ok(outcome) => Ok(WireOk::Submitted {
                outcome: wire_mutation_outcome(&outcome),
            }),
            Err(error) => {
                let mapped = wire_mutation_error(&error);
                Err((mapped.code, mapped.message))
            }
        }
    }

    /// Forward one pin to the loop-thread policy executor and block
    /// for its terminal answer. The interface interprets nothing:
    /// decode failures refuse at the wire, policy refusals carry
    /// through as distinct codes, and a success means exactly what an
    /// in-process pin's success means.
    pub(super) fn op_policy_pin(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-pin params: {error}"),
            )
        })?;
        reject_bound_non_read("policy-pin", &params)?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Pin { path: params.path })
        {
            Ok(PolicyResponse::Pinned(report)) => Ok(WireOk::PolicyPinned {
                pinned: report.pinned,
                already_pinned: report.already_pinned,
                files: report.files,
                dirs: report.dirs,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered pin with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }

    /// Forward one unpin to the loop-thread policy executor and block
    /// for its terminal answer. Releasing a promise never refuses —
    /// the executor owns that invariant, the interface only carries
    /// it.
    pub(super) fn op_policy_unpin(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-unpin params: {error}"),
            )
        })?;
        reject_bound_non_read("policy-unpin", &params)?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Unpin { path: params.path })
        {
            Ok(PolicyResponse::Unpinned(report)) => Ok(WireOk::PolicyUnpinned {
                released: report.released,
                already_unpinned: report.already_unpinned,
                files: report.files,
                dirs: report.dirs,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered unpin with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }

    /// Forward one evict to the loop-thread policy executor and block
    /// for its terminal answer. A pinned subtree refuses with
    /// `PinnedPresent` before committing anything — narrowing an
    /// explicit promise piecemeal never happens silently.
    pub(super) fn op_policy_evict(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-evict params: {error}"),
            )
        })?;
        reject_bound_non_read("policy-evict", &params)?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Evict { path: params.path })
        {
            Ok(PolicyResponse::Evicted(report)) => Ok(WireOk::PolicyEvicted {
                released: report.released,
                already_remote: report.already_remote,
                files: report.files,
                dirs: report.dirs,
            }),
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered evict with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }

    /// Answer one on-demand status read from the loop-thread executor.
    /// No periodic census exists: every status is a bounded walk at
    /// request time, with a `complete` flag naming whether the
    /// traversal budget covered the subtree. The generation and
    /// revision ride the loop's answer — captured from the same
    /// published generation the walk read — so the consumer
    /// correlates the answer with the freshness judgment instead of
    /// assuming recency.
    pub(super) fn op_policy_status(
        &self,
        params: serde_json::Value,
    ) -> Result<WireOk, (WireErrorCode, String)> {
        let params: PathParams = serde_json::from_value(params).map_err(|error| {
            (
                WireErrorCode::Malformed,
                format!("bad policy-status params: {error}"),
            )
        })?;
        reject_bound_non_read("policy-status", &params)?;
        match self
            .handles
            .policy
            .submit(PolicyRequest::Status { path: params.path })
        {
            Ok(PolicyResponse::Status {
                census: bounded,
                generation,
                revision,
            }) => {
                let census = &bounded.census;
                Ok(WireOk::PolicyStatus {
                    status: WirePolicyStatus {
                        files: census.files.iter().map(wire_policy_row).collect(),
                        conflicts: census.conflicts.clone(),
                        dirs: census.dirs,
                        symlinks_skipped: census.symlinks_skipped,
                        complete: bounded.complete,
                    },
                    generation,
                    revision,
                })
            }
            Ok(_) => Err((
                WireErrorCode::Engine,
                "policy channel answered status with the wrong outcome".to_string(),
            )),
            Err(error) => Err(map_policy_error(error)),
        }
    }
}
