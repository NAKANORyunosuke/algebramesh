//! `algebramesh verify <job-id>` (V4, ADR 0031): the record of a job and
//! the bodies of its blobs are read from the scheduler, checked against
//! their hashes, and the value is checked again with the checkers the agent
//! uses ([`algebramesh_agent::registry`], package checkers included). The
//! record is only read. A value whose certificate was left out is answered
//! "cannot be rechecked"; no value is ever printed.

use std::collections::BTreeMap;
use std::process::ExitCode;

use algebramesh_client::{Client, ClientError};
use algebramesh_core::blob::BlobRef;
use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{Claim, Guarantee};
use algebramesh_verify::recheck::{
    BlobStatus, Body, InputCheck, Outcome, Recheck, Recorded, blobs, recheck,
};
use clap::Args;

use crate::local::print_json;
use crate::remote::RemoteArgs;

#[derive(Args)]
pub struct VerifyArgs {
    job_id: String,
    /// Output format: table (for people) or json.
    #[arg(long, default_value = "table")]
    format: String,
    #[command(flatten)]
    remote: RemoteArgs,
}

/// The body of `r` as the scheduler gives it.
async fn fetch(client: &Client, r: &BlobRef) -> Body {
    match client.blob(r).await {
        Ok(bytes) => Body::Bytes(bytes),
        Err(ClientError::Rpc {
            code: tonic::Code::DataLoss,
            message,
        }) => Body::DataLoss(message),
        Err(ClientError::Data(message)) if message.contains("SHA-256") => {
            Body::HashMismatch(message)
        }
        Err(e) => Body::Unreadable(e.to_string()),
    }
}

/// Exit status 0 when the value passed the checks again, 1 when it did not
/// or could not be checked again, 2 when the record could not be read.
pub async fn verify(args: VerifyArgs) -> Result<ExitCode, String> {
    let json = match args.format.as_str() {
        "table" => false,
        "json" => true,
        other => return Err(format!("unsupported format {other:?}; use table or json")),
    };
    let client = args.remote.connect().await?;
    let (status, record) = client
        .result_refs(&args.job_id)
        .await
        .map_err(|e| e.to_string())?;
    let record = record.ok_or_else(|| {
        format!(
            "job {} is {}; it has no record to check yet",
            args.job_id, status.state
        )
    })?;
    let mut bodies = BTreeMap::new();
    for (_, r) in blobs(&record).unwrap_or_default() {
        if !bodies.contains_key(&r.id) {
            let body = fetch(&client, &r).await;
            bodies.insert(r.id.clone(), body);
        }
    }
    let answer = tokio::task::spawn_blocking(move || {
        recheck(
            &algebramesh_agent::registry(),
            &record,
            &bodies,
            &Interrupt::none(),
        )
    })
    .await
    .map_err(|e| format!("the check stopped: {e}"))?;
    if json {
        print_json(&answer, true)?;
    } else {
        print!("{}", table(&answer));
    }
    Ok(if answer.rechecked() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The snake_case name serde gives a level, a method, a summary or a status.
fn name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The level of the claim `c` of `g`, with its effective level when lower.
fn level(g: &Guarantee, c: &Claim) -> String {
    let mut level = name(&c.level);
    let effective = g.effective_level(&c.id);
    if effective != c.level {
        level = format!("{level} (effectively {})", name(&effective));
    }
    level
}

fn summary(label: &str, g: &Guarantee) -> String {
    format!(
        "{label}: {} ({} {})\n",
        name(&g.summary),
        g.verifier.name,
        g.verifier.version
    )
}

/// The claim `id` of `g`, with `g`.
fn find<'a>(g: Option<&'a Guarantee>, id: &str) -> Option<(&'a Guarantee, &'a Claim)> {
    g.and_then(|g| g.claims.iter().find(|c| c.id == id).map(|c| (g, c)))
}

/// `answer` for people: the record, its blobs, what it claims, what the
/// check found, and the claims side by side.
pub fn table(answer: &Recheck) -> String {
    let mut out = format!(
        "job: {} ({}, {})\n",
        answer.job_id,
        answer.operation,
        name(&answer.state)
    );
    out.push_str(&match &answer.input {
        InputCheck::Ok => "input: sha256 matches\n".to_owned(),
        InputCheck::HashMismatch(m) => format!("input: hash mismatch: {m}\n"),
        InputCheck::SpecMismatch(m) => format!("input: spec mismatch: {m}\n"),
    });
    for b in &answer.blobs {
        out.push_str(&format!(
            "blob {}: {} ({} bytes): {}",
            b.place,
            b.id,
            b.size,
            name(&b.status)
        ));
        if b.status != BlobStatus::Ok
            && let Some(m) = &b.message
        {
            out.push_str(&format!(": {m}"));
        }
        out.push('\n');
    }
    let recorded = answer.recorded.guarantee();
    match &answer.recorded {
        Recorded::Record { guarantee } => out.push_str(&summary("recorded", guarantee)),
        Recorded::LegacyMapping { guarantee } => out.push_str(&summary(
            "recorded (no guarantee in the record; legacy mapping)",
            guarantee,
        )),
        Recorded::LegacyRefused { error } => {
            out.push_str(&format!(
                "recorded: no guarantee; legacy mapping refused: {error}\n"
            ));
        }
        Recorded::Nothing { reason } => out.push_str(&format!("recorded: {reason}\n")),
    }
    let rechecked = match &answer.recheck {
        Outcome::Rechecked { guarantee } => {
            out.push_str(&summary("recheck", guarantee));
            Some(guarantee)
        }
        Outcome::CannotRecheck { reason } => {
            out.push_str(&format!("recheck: cannot be rechecked: {reason}\n"));
            None
        }
        Outcome::Refused { error } => {
            out.push_str(&format!("recheck: refused: {error}\n"));
            None
        }
        Outcome::NotChecked { reason } => {
            out.push_str(&format!("recheck: not done: {reason}\n"));
            None
        }
    };
    // The claims of the check, then those only the record has.
    let mut ids: Vec<&str> = Vec::new();
    for g in [rechecked, recorded].into_iter().flatten() {
        for c in &g.claims {
            if !ids.contains(&c.id.as_str()) {
                ids.push(&c.id);
            }
        }
    }
    if ids.is_empty() {
        return out;
    }
    let mut rows: Vec<[String; 5]> = Vec::new();
    let mut differ = 0;
    for id in &ids {
        let old = find(recorded, id);
        let new = find(rechecked, id);
        let old_level = old.map_or_else(|| "-".to_owned(), |(g, c)| level(g, c));
        let new_level = new.map_or_else(|| "-".to_owned(), |(g, c)| level(g, c));
        if rechecked.is_some() && old_level != new_level {
            differ += 1;
        }
        let Some((_, shown)) = new.or(old) else {
            continue;
        };
        rows.push([
            (*id).to_owned(),
            old_level,
            new_level,
            shown.method.as_ref().map_or_else(|| "-".to_owned(), name),
            shown.by.clone(),
        ]);
    }
    let header = ["claim", "recorded", "rechecked", "method", "by"].map(String::from);
    let widths: Vec<usize> = (0..5)
        .map(|i| {
            rows.iter()
                .chain(std::iter::once(&header))
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    if rechecked.is_some() {
        if differ == 0 {
            out.push_str("levels: as recorded\n");
        } else {
            out.push_str(&format!(
                "levels: {differ} of {} claims differ from the record\n",
                ids.len()
            ));
        }
    }
    if let Some(g) = rechecked.or(recorded) {
        for p in &g.not_claimed {
            out.push_str(&format!("not claimed: {p}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use algebramesh_core::JobState;
    use algebramesh_core::blob::BlobId;
    use algebramesh_core::guarantee::{EvidenceLevel, Method, Scope, VerifierInfo};
    use algebramesh_verify::recheck::BlobCheck;

    use super::*;

    fn guarantee(levels: &[(&str, EvidenceLevel)]) -> Guarantee {
        Guarantee::new(
            VerifierInfo {
                name: "algebramesh-verify".into(),
                version: "1.1.0".into(),
                commit: None,
            },
            None,
            levels
                .iter()
                .map(|(id, level)| Claim {
                    id: (*id).into(),
                    level: *level,
                    method: (*level == EvidenceLevel::Checked).then_some(Method::SelfCertifying),
                    by: "someone".into(),
                    scope: Scope::Exact,
                    depends_on: vec![],
                    reason: None,
                })
                .collect(),
            vec![],
        )
    }

    fn answer(recorded: Recorded, recheck: Outcome) -> Recheck {
        Recheck {
            job_id: "j".into(),
            operation: "polynomial.factor".into(),
            state: JobState::Succeeded,
            input: InputCheck::Ok,
            blobs: vec![BlobCheck {
                place: "result".into(),
                id: BlobId::of(b"x").to_string(),
                size: 1,
                status: BlobStatus::Ok,
                message: None,
            }],
            recorded,
            recheck,
        }
    }

    #[test]
    fn the_table_sets_the_recorded_levels_beside_the_rechecked_ones() {
        use EvidenceLevel::*;
        let table = table(&answer(
            Recorded::LegacyMapping {
                guarantee: guarantee(&[("a", Checked), ("b", Asserted)]),
            },
            Outcome::Rechecked {
                guarantee: guarantee(&[("a", Checked), ("b", Checked)]),
            },
        ));
        assert!(table.contains("input: sha256 matches"), "{table}");
        assert!(table.contains("blob result: sha256:"), "{table}");
        assert!(
            table.contains("recorded (no guarantee in the record; legacy mapping)"),
            "{table}"
        );
        let b = table.lines().find(|l| l.starts_with("b ")).unwrap();
        assert!(b.contains("asserted") && b.contains("checked"), "{table}");
        assert!(table.contains("levels: 1 of 2 claims differ"), "{table}");
    }

    #[test]
    fn a_value_that_cannot_be_rechecked_shows_only_what_was_recorded() {
        let table = table(&answer(
            Recorded::Record {
                guarantee: guarantee(&[("a", EvidenceLevel::Asserted)]),
            },
            Outcome::CannotRecheck {
                reason: "the certificate of the value was left out".into(),
            },
        ));
        assert!(table.contains("recheck: cannot be rechecked"), "{table}");
        let a = table.lines().find(|l| l.starts_with("a ")).unwrap();
        assert!(a.contains("asserted") && a.contains(" - "), "{table}");
        assert!(!table.contains("levels:"), "{table}");
    }
}
