//! The scorecard, kept apart from the probes that fill it.
//!
//! Rendering states what happened and does not grade it: a server that
//! answers two of six is reported as two of six, with the failures named,
//! because which two is the whole point. An average would hide it.

/// Seconds since the Unix epoch, as the provenance date. A human calendar
/// date would need a formatting dependency for one field; the epoch is
/// unambiguous and converts losslessly.
fn chrono_date() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The machine that measured, from the environment when set and `uname -n`
/// otherwise. Absent rather than guessed: `None` when neither answers.
fn machine_name() -> Option<String> {
    if let Ok(name) = std::env::var("HOSTNAME").map(|name| name.trim().to_string())
        && !name.is_empty()
    {
        return Some(name);
    }
    std::process::Command::new("uname")
        .arg("-n")
        .output()
        .ok()
        .and_then(|output| {
            std::str::from_utf8(&output.stdout)
                .ok()
                .map(|text| text.trim().to_string())
        })
        .filter(|text| !text.is_empty())
}

/// What one probe got back.
pub enum Outcome {
    /// The answer was what the probe asked for.
    Answered,
    /// The tool answered, but not with what the probe needed. Carries the
    /// first line, which is where structural puts "no such symbol" and the
    /// like.
    Unmet(String),
    /// The tool returned an error rather than an answer.
    Failed(String),
}

impl Outcome {
    /// Judge a tool's result by whether its text holds what the probe
    /// needed.
    ///
    /// An `Err` is kept distinct from an answer that missed: a server that
    /// timed out and a server that confidently returned the wrong symbol
    /// are different findings, and collapsing them would hide the second.
    pub fn judge(answer: &anyhow::Result<String>, satisfied: impl Fn(&str) -> bool) -> Outcome {
        match answer {
            Ok(text) if satisfied(text) => Outcome::Answered,
            Ok(text) => Outcome::Unmet(first_line(text)),
            Err(error) => Outcome::Failed(
                format!("{error:#}")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string(),
            ),
        }
    }

    fn mark(&self) -> &'static str {
        match self {
            Outcome::Answered => "yes",
            Outcome::Unmet(_) => "no",
            Outcome::Failed(_) => "err",
        }
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.len() > 60 {
        format!("{}…", &line[..59])
    } else {
        line.to_string()
    }
}

enum Row {
    Scored {
        name: String,
        argv: String,
        version: String,
        outcomes: Vec<Outcome>,
    },
    /// On `PATH` or not, but never asked. `why` is present when the
    /// binary exists and would not start — a different finding from
    /// "not installed", and the one that decides whether walking a
    /// preference list can rely on a lookup.
    Absent {
        name: String,
        argv: String,
        why: Option<String>,
    },
}

/// A trimmed `git` answer, or `None` when git could not be run.
fn git(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    let text = std::str::from_utf8(&output.stdout).ok()?.trim().to_string();
    (!text.is_empty() || args[0] == "status").then_some(text)
}

pub struct Scorecard {
    language: String,
    probes: Vec<&'static str>,
    /// The fixture tree the probes ran against
    /// (`structural::fixtures::Fixture::revision`). The trees are
    /// materialized rather than resident, so a score names the tree it
    /// measured the same way it names the server version.
    fixture: String,
    rows: Vec<Row>,
}

impl Scorecard {
    pub fn new(language: &str, probes: Vec<&'static str>, fixture: String) -> Self {
        Scorecard {
            language: language.to_string(),
            probes,
            fixture,
            rows: Vec::new(),
        }
    }

    /// The checked-in artifact `catalogue-synthesis.md` requires: per-probe
    /// outcomes (not just the total), with provenance on every row. A flat
    /// count out of eight weighs a verified rename the same as a hit line's
    /// container; storing the vector means a weighting can be introduced
    /// later without reinstalling and re-running every server. A field
    /// whose provenance cannot be stated is absent rather than guessed —
    /// `None` serializes as missing, never as an empty string pretending to
    /// be a version.
    pub fn write_json(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let structural_revision = git(&["rev-parse", "HEAD"]).map(|head| {
            // A score is a joint measurement of server *and client*
            // (`../../src/discovery/catalogue.rs`), so a run from a working
            // tree that is not the commit says so: `HEAD` alone would name
            // code that did not produce these numbers.
            match git(&["status", "--porcelain"]) {
                Some(changes) if !changes.is_empty() => format!("{head}-dirty"),
                _ => head,
            }
        });
        let document = serde_json::json!({
            "language": self.language,
            "fixture_revision": self.fixture,
            "structural_revision": structural_revision,
            "date": chrono_date(),
            "machine": machine_name(),
            "probes": self.probes,
            "rows": self.rows.iter().map(|row| match row {
                Row::Scored { name, argv, version, outcomes } => serde_json::json!({
                    "candidate": name,
                    "argv": argv,
                    "version": if version.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(version.clone()) },
                    "outcomes": outcomes.iter().map(|outcome| match outcome {
                        Outcome::Answered => serde_json::json!({"probe_answered": true}),
                        Outcome::Unmet(detail) => serde_json::json!({"probe_answered": false, "kind": "unmet", "detail": detail}),
                        Outcome::Failed(detail) => serde_json::json!({"probe_answered": false, "kind": "failed", "detail": detail}),
                    }).collect::<Vec<_>>(),
                }),
                Row::Absent { name, argv, why } => serde_json::json!({
                    "candidate": name,
                    "argv": argv,
                    "absent": true,
                    "unrunnable": why,
                }),
            }).collect::<Vec<_>>(),
        });
        std::fs::write(path, serde_json::to_string_pretty(&document)?)?;
        Ok(())
    }

    pub fn scored(&mut self, name: &str, argv: String, version: String, outcomes: Vec<Outcome>) {
        self.rows.push(Row::Scored {
            name: name.to_string(),
            argv,
            version,
            outcomes,
        });
    }

    pub fn absent(&mut self, name: &str, argv: String, why: Option<String>) {
        self.rows.push(Row::Absent {
            name: name.to_string(),
            argv,
            why,
        });
    }

    pub fn render(&self, needs: Vec<&'static str>) -> String {
        let mut text = format!("\n# Conformance: {}\n\n", self.language);
        text.push_str(
            "Each cell is one observation on the server and version that \
             ran — not validation, and not a general guarantee \
             (process/testing.md). What a server advertises is not measured \
             here; what it answered is.\n\n",
        );
        text.push_str(&format!(
            "Fixture tree: `{}` · revision `{}` (materialized from \
             src/fixtures.rs, not resident in the repository).\n\n",
            self.language, self.fixture
        ));

        text.push_str("| probe | needs |");
        for row in &self.rows {
            if let Row::Scored { name, .. } = row {
                text.push_str(&format!(" {name} |"));
            }
        }
        text.push_str("\n| --- | --- |");
        for row in &self.rows {
            if matches!(row, Row::Scored { .. }) {
                text.push_str(" --- |");
            }
        }
        text.push('\n');

        for (index, probe) in self.probes.iter().enumerate() {
            text.push_str(&format!("| {probe} | {} |", needs[index]));
            for row in &self.rows {
                if let Row::Scored { outcomes, .. } = row {
                    text.push_str(&format!(
                        " {} |",
                        outcomes.get(index).map(Outcome::mark).unwrap_or("—")
                    ));
                }
            }
            text.push('\n');
        }

        text.push_str("| **answered** | |");
        for row in &self.rows {
            if let Row::Scored { outcomes, .. } = row {
                let answered = outcomes
                    .iter()
                    .filter(|o| matches!(o, Outcome::Answered))
                    .count();
                text.push_str(&format!(" **{answered}/{}** |", outcomes.len()));
            }
        }
        text.push_str("\n\n");

        for row in &self.rows {
            if let Row::Scored {
                name,
                argv,
                version,
                outcomes,
            } = row
            {
                text.push_str(&format!(
                    "- `{name}` — `{argv}`{}\n",
                    if version.is_empty() {
                        String::new()
                    } else {
                        format!(" · {version}")
                    }
                ));
                // Why a probe came up short matters more than that it did:
                // "no such symbol" and a transport error are different
                // problems with the same score.
                for (index, outcome) in outcomes.iter().enumerate() {
                    match outcome {
                        Outcome::Unmet(detail) => text.push_str(&format!(
                            "    - {}: answered, but not with it — {detail}\n",
                            self.probes[index]
                        )),
                        Outcome::Failed(detail) => text
                            .push_str(&format!("    - {}: error — {detail}\n", self.probes[index])),
                        Outcome::Answered => {}
                    }
                }
            }
        }

        let mut missing = Vec::new();
        let mut broken = Vec::new();
        for row in &self.rows {
            if let Row::Absent { name, argv, why } = row {
                match why {
                    Some(why) => broken.push(format!("`{name}` (`{argv}`): {why}")),
                    None => missing.push(format!("`{name}` (`{argv}`)")),
                }
            }
        }
        if !broken.is_empty() {
            text.push_str(
                "\nOn PATH and would not start — presence is not \
                 availability, and a preference walk that stopped at the \
                 lookup would stop here forever:\n",
            );
            for entry in &broken {
                text.push_str(&format!("- {entry}\n"));
            }
        }
        if !missing.is_empty() {
            text.push_str(&format!("\nNot installed: {}\n", missing.join(", ")));
        }
        text
    }
}
