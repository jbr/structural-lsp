//! A workspace's configuration: which language servers to run. Read from
//! `structural.toml` at the workspace root, or handed over as a typed
//! [`Config`] by an embedder through [`Source`].
//!
//! ```toml
//! idle-eviction-secs = 1800        # release a server no query has
//!                                  # addressed for this long (default 1800)
//!
//! [servers.rust-analyzer]          # a built-in: override its command or backstop
//! command = ["/opt/ra/rust-analyzer"]
//! backstop-secs = 180
//!
//! [servers.gopls]                  # any other name: a generic server
//! command = ["gopls", "serve"]
//! languages = ["go"]               # language ids; extensions from the table
//!                                  # below, or:
//! extensions = { go = ["go"] }
//!
//! [servers.pyright]
//! disabled = true                  # not in this project, built-in or not
//!
//! [servers.ccls]
//! command = ["ccls"]
//! languages = ["cpp"]
//! progress-create-is-work = false  # reserves a progress token it never uses
//! ```
//!
//! Built-in adapters are on by default; an entry with a built-in's name
//! overrides its command, languages, or backstop and keeps everything else
//! (readiness policy, capabilities, query rewriting). Absent file: the
//! built-ins alone.
//!
//! This file is the **override layer** over the server catalogue
//! (`docs/discovery.md`), not the whole configuration: naming a server
//! configures it, `disabled = true` takes it out of consideration, and a
//! name the catalogue does not know is added generically. What actually
//! starts is what selection plans from the tree's evidence, so a
//! configured server whose language is not in the tree lays out no slot.
//!
//! The file is one spelling of a [`Config`]. An embedder whose own
//! configuration carries these keys builds the value itself — by
//! deserializing its own file into it, or field by field — and returns it
//! from the [`Source`] it gives the host, and publishes the keys' schema
//! from the type's `JsonSchema` (`docs/servers.md`). Keys are kebab-case,
//! as the embedder's are. Everything after loading treats both alike.

use crate::discovery::catalogue::{self, Entry};
use crate::discovery::selection::Overrides;
use crate::lsp::adapter::{self, Adapter, DEFAULT_BACKSTOP, Language, configured::Configured};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

mod schema;
pub use schema::{Config, DEFAULT_IDLE_EVICTION, ServerEntry};

pub const FILE_NAME: &str = "structural.toml";

/// Where each workspace's configuration comes from: a function of the
/// workspace's canonical root, called once when the host creates the
/// workspace and under the host's map lock, so it must not call back into
/// the host.
///
/// A function of the root alone, because the host keys a workspace by its
/// root and every caller on that root shares it: a source that answered
/// differently per caller would hand every later caller the configuration
/// of whichever one arrived first (`docs/ownership.md`). An error is kept
/// as the workspace's configuration error — nothing starts, and every call
/// says why — exactly as a `structural.toml` that does not parse is.
pub type Source = Arc<dyn Fn(&Path) -> Result<Config> + Send + Sync>;

/// The source the front end uses: [`Config::load`], the `structural.toml`
/// at the root.
pub fn from_file() -> Source {
    Arc::new(Config::load)
}

/// Extensions for language ids a user is likely to name without spelling
/// them out. Anything else needs `extensions`.
///
/// The table itself lives in the catalogue
/// (`docs/discovery.md`), which is where a server's languages
/// are data rather than a branch in a parser.
fn default_extensions(language: &str) -> Option<&'static [&'static str]> {
    crate::discovery::catalogue::extensions_for_language(language)
}

impl Config {
    /// The config at `root`, or the empty default when there is none. A
    /// present but unparsable file is an error: silently ignoring it would
    /// run the wrong servers.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The adapters this file defines, and the override layer it puts over
    /// the catalogue.
    ///
    /// The two are produced together because they have to agree about
    /// names: a plan names a catalogue entry, and a slot is laid out for
    /// the adapter of that name. A disabled entry produces neither, which
    /// is also why it is never validated — `disabled = true` on a generic
    /// server with no `command` is a statement about this project, not an
    /// incomplete definition.
    pub fn layout(self) -> Result<Layout> {
        let mut entries = self.servers;
        let mut overrides = Overrides::new();
        let mut adapters: Vec<Arc<dyn Adapter>> = Vec::new();

        let mut configured: Vec<(String, bool)> = Vec::new();
        for builtin in adapter::builtins() {
            match entries.remove(builtin.name()) {
                Some(entry) if entry.disabled => {
                    overrides = overrides.disable(builtin.name());
                }
                Some(entry) => {
                    let name = builtin.name().to_string();
                    adapters.push(Arc::new(override_builtin(builtin, entry)?));
                    configured.push((name, true));
                }
                None => adapters.push(builtin),
            }
        }
        for (name, entry) in entries {
            if entry.disabled {
                overrides = overrides.disable(&name);
                continue;
            }
            adapters.push(Arc::new(generic(name.clone(), entry)?));
            configured.push((name, true));
        }

        // An entry the file named replaces whatever the catalogue said
        // about it, because the merged adapter is what will actually run
        // and its languages are what a plan has to route by.
        for (name, _) in &configured {
            if let Some(adapter) = adapters.iter().find(|a| a.name() == name) {
                overrides = overrides.configure(entry_for(adapter.as_ref()));
            }
        }
        Ok(Layout {
            adapters,
            overrides,
        })
    }
}

/// What `structural.toml` produces: the adapters it defines and the
/// override layer it puts over the catalogue.
pub struct Layout {
    pub adapters: Vec<Arc<dyn Adapter>>,
    pub overrides: Overrides,
}

impl Layout {
    /// The adapter this file (or a built-in) defines for `name`.
    ///
    /// Preferred over [`adapter_for`] wherever both could answer, because
    /// this one carries what the catalogue entry cannot: a built-in's
    /// protocol behaviour, and whatever the file overrode on it.
    pub fn adapter(&self, name: &str) -> Option<Arc<dyn Adapter>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.name() == name)
            .map(Arc::clone)
    }
}

/// A catalogue entry describing an adapter, so selection can route by the
/// languages that adapter will actually serve.
///
/// It carries no score: a server named in a project's configuration has
/// not been measured, and inventing a number for it here would be the
/// thing the catalogue exists to prevent (`docs/discovery.md`).
fn entry_for(adapter: &dyn Adapter) -> Entry {
    Entry::configured(
        adapter.name(),
        &adapter.command(),
        adapter
            .languages()
            .iter()
            .map(|language| {
                catalogue::Serves::new(
                    &language.id,
                    &language
                        .extensions
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
    )
}

/// The adapter that runs a catalogue entry `structural.toml` did not
/// define: the inverse of `entry_for` above, and what lets a plan's
/// **alternative** be started at all.
///
/// A plan's ladder names catalogue entries, and only the ones the file or
/// the built-ins already describe have an adapter waiting in the
/// [`Layout`]. The rest — a fallback candidate this project never
/// configured — need one built from the entry itself: its command, and the
/// languages it claims.
///
/// `base` is `None` deliberately: a built-in always arrives through the
/// layout, because a built-in the file disabled is removed from the
/// catalogue and can never be a candidate here.
pub fn adapter_for(entry: &Entry) -> Arc<dyn Adapter> {
    Arc::new(Configured {
        name: entry.name.clone(),
        command: entry.command.clone(),
        languages: entry
            .serves
            .iter()
            .map(|serves| Language {
                id: serves.language.clone(),
                extensions: serves.extensions.clone(),
            })
            .collect(),
        backstop: DEFAULT_BACKSTOP,
        base: None,
        configuration: None,
        progress_create_is_work: true,
    })
}

fn languages_of(name: &str, entry: &ServerEntry) -> Result<Vec<Language>> {
    entry
        .languages
        .iter()
        .map(|id| {
            let extensions: Vec<String> = match entry.extensions.get(id) {
                Some(extensions) => extensions.clone(),
                None => default_extensions(id)
                    .map(|e| e.iter().map(|s| s.to_string()).collect())
                    .with_context(|| {
                        format!(
                            "[servers.{name}]: no known file extensions for language `{id}`; \
                             add `extensions = {{ {id} = [\"…\"] }}`"
                        )
                    })?,
            };
            Ok(Language {
                id: id.clone(),
                extensions,
            })
        })
        .collect()
}

fn override_builtin(builtin: Arc<dyn Adapter>, entry: ServerEntry) -> Result<Configured> {
    let name = builtin.name().to_string();
    let command = if entry.command.is_empty() {
        builtin.command()
    } else {
        entry.command.clone()
    };
    let languages = if entry.languages.is_empty() {
        builtin.languages()
    } else {
        languages_of(&name, &entry)?
    };
    let backstop = entry
        .backstop_secs
        .map(Duration::from_secs)
        .unwrap_or_else(|| builtin.backstop());
    let progress_create_is_work = entry
        .progress_create_is_work
        .unwrap_or_else(|| builtin.progress_create_is_work());
    Ok(Configured {
        name,
        command,
        languages,
        backstop,
        base: Some(builtin),
        configuration: entry.settings,
        progress_create_is_work,
    })
}

fn generic(name: String, entry: ServerEntry) -> Result<Configured> {
    if entry.command.is_empty() {
        bail!("[servers.{name}]: `command` is required for a server that is not built in");
    }
    if entry.languages.is_empty() {
        bail!("[servers.{name}]: `languages` is required for a server that is not built in");
    }
    let languages = languages_of(&name, &entry)?;
    Ok(Configured {
        name,
        command: entry.command,
        languages,
        backstop: entry
            .backstop_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_BACKSTOP),
        base: None,
        configuration: entry.settings,
        progress_create_is_work: entry.progress_create_is_work.unwrap_or(true),
    })
}

#[cfg(test)]
mod tests;
