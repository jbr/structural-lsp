//! Emit the shipped catalogue from the checked-in evidence — the one step
//! between the conformance runs and the entries `Catalogue::built_in`
//! serves.
//!
//! ```sh
//! cargo run --bin catalogue
//! ```
//!
//! Inputs: every `tests/conformance/scores/<language>.json` — the
//! per-probe evidence, with its provenance on every row — and
//! `reference/servers-list/by_executable.json`, the dataset's
//! per-executable reduction, from which the consensus tiebreak is counted:
//! every source recording the executable for any language, minus
//! `mason-registry`, an installer index rather than a selector
//! (`docs/catalogue.md`). The output is
//! `src/discovery/catalogue/generated.rs`, checked in, so a change to the
//! shipped catalogue is a reviewable diff.
//!
//! The generator follows the synthesis rather than replacing it
//! (`docs/catalogue.md`): it emits exactly what was measured — no
//! threshold, no additions — and refuses to carry a field whose provenance
//! it cannot state. A version that reads as anything but a version is
//! recorded blank, the way the probe recorded it; an executable the
//! dataset does not name is an error, not a silent zero consensus.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// An installer index, not a selector: it says a server can be installed,
/// not that an editor ships it (`reference/servers-list/README.md`).
const NOT_A_SELECTOR: &str = "mason-registry";

/// The consensus count for `executable`: sources that record it, for any
/// language, minus [`NOT_A_SELECTOR`].
fn consensus(by_executable: &BTreeMap<String, Value>, executable: &str) -> Result<u16> {
    let record = by_executable.get(executable).with_context(|| {
        format!("consensus for {executable} has no provenance: not named by the dataset (reference/servers-list/by_executable.json)")
    })?;
    let sources = record["sources"]
        .as_array()
        .with_context(|| format!("`sources` is not an array for {executable}"))?;
    Ok(sources
        .iter()
        .filter_map(|s| s.as_str())
        .filter(|s| *s != NOT_A_SELECTOR)
        .count() as u16)
}

/// A version that reads as anything but a version is blank rather than
/// guessed: the probe recorded what the server printed, and the catalogue
/// carries only what can be stated as a version. A version string has a
/// digit; a traceback does not.
fn version(raw: Option<&str>) -> String {
    match raw {
        Some(raw) if raw.chars().any(|c| c.is_ascii_digit()) => raw.to_string(),
        _ => String::new(),
    }
}

/// The UTC date of an epoch-second timestamp, as `YYYY-MM-DD`.
///
/// Days-since-epoch to civil date by the standard arithmetic (Hinnant's
/// `civil_from_days`), because the crate has no calendar dependency and a
/// generator that needs one to print a date it already has is wrong.
fn iso_date(epoch_seconds: i64) -> Result<String> {
    let days = epoch_seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

/// One checked-in row, rendered as an `Entry` literal.
fn entry(
    language: &str,
    row: &Value,
    provenance: &Provenance,
    by_executable: &BTreeMap<String, Value>,
) -> Result<String> {
    let candidate = row["candidate"]
        .as_str()
        .with_context(|| "row has no `candidate`")?;
    let argv = row["argv"]
        .as_str()
        .with_context(|| format!("{candidate} has no `argv`"))?;
    let command: Vec<String> = argv.split_whitespace().map(str::to_string).collect();
    let executable = command.first().expect("argv is non-empty");
    let outcomes = row["outcomes"]
        .as_array()
        .with_context(|| format!("{candidate} has no `outcomes`"))?;
    let probes = outcomes.len() as u8;
    let answered = outcomes
        .iter()
        .filter(|o| o["probe_answered"].as_bool() == Some(true))
        .count() as u8;
    Ok(format!(
        concat!(
            "        Entry {{\n",
            "            name: {candidate}.to_string(),\n",
            "            command: [{command}].iter().map(|c| c.to_string()).collect(),\n",
            "            serves: vec![Serves::known({language}).expect(\"language is in LANGUAGES\")],\n",
            "            score: Some(Score {{\n",
            "                answered: {answered},\n",
            "                probes: {probes},\n",
            "                measurement: Measurement {{\n",
            "                    server_version: {server_version}.to_string(),\n",
            "                    structural_revision: {structural_revision}.to_string(),\n",
            "                    fixture_revision: {fixture_revision}.to_string(),\n",
            "                    date: {date}.to_string(),\n",
            "                }},\n",
            "            }}),\n",
            "            consensus: {consensus},\n",
            "            provenance: Provenance {{\n",
            "                source: {source}.to_string(),\n",
            "                extensions: Confidence::Exact,\n",
            "            }},\n",
            "        }},\n",
        ),
        candidate = rust_string(candidate),
        command = rust_strings(&command),
        language = rust_string(language),
        answered = answered,
        probes = probes,
        server_version = rust_string(&version(row["version"].as_str())),
        structural_revision = rust_string(&provenance.structural_revision),
        fixture_revision = rust_string(&provenance.fixture_revision),
        date = rust_string(&provenance.date),
        consensus = consensus(by_executable, executable)?,
        source = rust_string(&provenance.source),
    ))
}

/// A rust string literal for `raw`, quoted and escaped.
fn rust_string(raw: &str) -> String {
    format!("{raw:?}")
}

/// A rust slice literal of string literals: `&["a", "b"]` rendered as
/// `"a", "b"` for a `vec![]`.
fn rust_strings(raws: &[String]) -> String {
    raws.iter()
        .map(|raw| rust_string(raw))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The provenance one score file states, shared by its rows.
struct Provenance {
    structural_revision: String,
    fixture_revision: String,
    date: String,
    source: String,
}

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scores = root.join("tests/conformance/scores");
    let dataset = root.join("reference/servers-list/by_executable.json");
    let output = root.join("src/discovery/catalogue/generated.rs");

    let by_executable: BTreeMap<String, Value> = serde_json::from_str(
        &std::fs::read_to_string(&dataset).with_context(|| dataset.display().to_string())?,
    )
    .with_context(|| dataset.display().to_string())?;

    let mut files = Vec::new();
    collect(&scores, &mut files)?;
    if files.is_empty() {
        bail!("no score files under {}", scores.display());
    }

    // Sorted so the generated file is a pure function of the evidence, and
    // a re-run that found the same evidence writes the same bytes.
    files.sort();
    let mut names_seen = BTreeMap::new();
    let mut entries = String::new();
    let mut languages = Vec::new();
    for path in &files {
        let file: Value = serde_json::from_str(
            &std::fs::read_to_string(path).with_context(|| path.display().to_string())?,
        )
        .with_context(|| path.display().to_string())?;
        let language = file["language"]
            .as_str()
            .with_context(|| format!("{} has no `language`", path.display()))?;
        let structural_revision = file["structural_revision"]
            .as_str()
            .with_context(|| format!("{} has no `structural_revision`", path.display()))?
            .to_string();
        let fixture_revision = file["fixture_revision"]
            .as_str()
            .with_context(|| format!("{} has no `fixture_revision`", path.display()))?
            .to_string();
        let date = iso_date(
            file["date"]
                .as_i64()
                .with_context(|| format!("{} has no epoch `date`", path.display()))?,
        )?;
        let provenance = Provenance {
            structural_revision,
            fixture_revision,
            date,
            source: format!(
                "tests/conformance/scores/{language}.json + reference/servers-list/by_executable.json"
            ),
        };
        let rows = file["rows"]
            .as_array()
            .with_context(|| format!("{} has no `rows`", path.display()))?;
        for row in rows {
            let name = row["candidate"]
                .as_str()
                .with_context(|| format!("a row of {} has no `candidate`", path.display()))?
                .to_string();
            if let Some(previous) = names_seen.insert(name.clone(), language.to_string()) {
                bail!(
                    "{name} is measured in both {previous} and {language}; a catalogue is keyed by name and could not carry both"
                );
            }
            entries.push_str(&entry(language, row, &provenance, &by_executable)?);
        }
        languages.push(language.to_string());
    }

    let generated = format!(
        "//! Generated by `cargo run --bin catalogue` from the checked-in\n\
         //! evidence — do not edit by hand; re-run the generator instead.\n\
         //!\n\
         //! Inputs: `tests/conformance/scores/<language>.json` (the per-probe\n\
         //! evidence, one run per language, provenance on every row) and\n\
         //! `reference/servers-list/by_executable.json` (the consensus\n\
         //! tiebreak: sources minus `mason-registry`). The rules this\n\
         //! implements, and the cuts this set rests on, are\n\
         //! `docs/catalogue.md`.\n\n\
         use super::{{Confidence, Entry, Measurement, Provenance, Score, Serves}};\n\n\
         /// Every measured server of every language the harness covers, in\n\
         /// name order — the name order is not the rank order; [`Entry::rank`]\n\
         /// supplies that.\n\
         pub(crate) fn entries() -> Vec<Entry> {{\n\
         \x20   vec![\n\
         {entries}\
         \x20   ]\n\
         }}\n",
    );
    std::fs::write(&output, generated).with_context(|| output.display().to_string())?;
    println!(
        "{} entries across {} languages -> {}",
        names_seen.len(),
        languages.len(),
        output.display()
    );
    Ok(())
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory).with_context(|| directory.display().to_string())? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "json") {
            files.push(path);
        }
    }
    Ok(())
}
