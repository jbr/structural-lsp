//! The queries: every request whose answer is read from the server's index
//! goes through [`LspServer::query`], so each is written only while the
//! server has no outstanding work and asked again if it answers while work
//! begun since is outstanding (docs/correctness.md).

use super::*;

impl LspServer {
    /// A semantic request, on the transport's guarded path
    /// ([`ServerHandle::query`]), with the adapter's reading of `null`.
    ///
    /// A `null` the adapter declares to mean "index not loaded" for this
    /// method is an error naming the server rather than an empty answer: by
    /// the time it arrives here the transport has seen no work begin since
    /// the request was written, so waiting again has nothing to wait for,
    /// and reading it as "no results" is the silent wrong answer the guard
    /// exists to prevent.
    pub(crate) fn query(&self, method: &str, params: Value) -> Result<Value> {
        let result = self.handle().query(method, params)?;
        if result.is_null() && self.adapter.null_is_unloaded(method) {
            bail!(
                "{} answered {method} with null, which it sends while its index is not loaded \
                 (its answer with nothing to report is []), and reported no work in progress; \
                 the answer was refused rather than read as no results",
                self.name()
            );
        }
        Ok(result)
    }

    /// `workspace/symbol`, with the adapter's query rewriting, decoded into
    /// candidates as the server reported them (no completeness claim is
    /// made anywhere downstream).
    ///
    /// Empty when the server did not advertise the capability, the same way
    /// [`Self::document_symbol`] answers `None`. Sending it anyway earns a
    /// `-32601`, and that error used to end the whole query: pylsp 1.15.0
    /// serves documents but has no symbol index, and a *file-scoped* query
    /// that never needed the index failed on it. A server that cannot
    /// nominate from an index contributes no candidates; the other servers
    /// still do, and the file's own outline still does.
    pub fn workspace_symbol(&self, query: &str) -> Result<Vec<SymbolInformation>> {
        if !self.supports("workspaceSymbolProvider") {
            return Ok(Vec::new());
        }
        let query = self.adapter.symbol_query(query);
        let result = self.query("workspace/symbol", json!({ "query": query }))?;
        let infos: Option<Vec<SymbolInformation>> =
            serde_json::from_value(result).context("parsing workspace/symbol result")?;
        Ok(infos.unwrap_or_default())
    }

    /// The adapter's `workspace/symbol` cap, when an answer of `count`
    /// results reached it: the answer is then a prefix of the matches, and
    /// what it lacks says nothing about absence (docs/resolution.md).
    pub fn symbol_cap_reached(&self, count: usize) -> Option<usize> {
        self.adapter.symbol_result_cap().filter(|cap| count >= *cap)
    }

    /// `textDocument/definition` at a position (in this server's encoding);
    /// the raw result is normalized by
    /// [`crate::lsp::definition::parse_definition_result`].
    pub fn definition(&self, uri: &str, position: Position) -> Result<Value> {
        self.query(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
            }),
        )
    }

    /// `textDocument/declaration` at a position, in the same shape as
    /// [`LspServer::definition`]: a header language has two sites for one
    /// entity, and a server that answers both distinguishes them
    /// (docs/resolution.md).
    pub fn declaration(&self, uri: &str, position: Position) -> Result<Value> {
        self.query(
            "textDocument/declaration",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
            }),
        )
    }

    /// `textDocument/references` at a position (in this server's encoding),
    /// without the declaration; the raw result is normalized by
    /// [`crate::lsp::references::parse_references_result`].
    pub fn references(&self, uri: &str, position: Position) -> Result<Value> {
        self.query(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
                "context": { "includeDeclaration": false },
            }),
        )
    }

    /// Any request, for adapter-defined extension views; the standard
    /// queries have their own methods. Guarded like them: an extension
    /// view reads the same index.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.query(method, params)
    }

    /// `textDocument/implementation` at a position; a `Location[]` (or
    /// link) result in the same shapes as `definition`.
    pub fn implementation(&self, uri: &str, position: Position) -> Result<Value> {
        self.query(
            "textDocument/implementation",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    /// `textDocument/prepareCallHierarchy` at a position: the
    /// `CallHierarchyItem[]` the direction queries take.
    pub fn prepare_call_hierarchy(&self, uri: &str, position: Position) -> Result<Value> {
        self.query(
            "textDocument/prepareCallHierarchy",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    /// `callHierarchy/incomingCalls` or `outgoingCalls` for one item.
    pub fn calls(&self, incoming: bool, item: &Value) -> Result<Value> {
        let method = if incoming {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        self.query(method, json!({ "item": item }))
    }

    /// `textDocument/hover` over `range`: at its start, or over the whole
    /// range when the adapter says the server takes one.
    pub fn hover(&self, uri: &str, range: lsp_types::Range) -> Result<Value> {
        self.query("textDocument/hover", self.adapter.hover_params(uri, range))
    }

    /// `textDocument/prepareRename` at a position: `null` when the server
    /// says nothing there can be renamed.
    pub fn prepare_rename(&self, uri: &str, position: Position) -> Result<Value> {
        self.query(
            "textDocument/prepareRename",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    /// `textDocument/rename`: the `WorkspaceEdit` that renames the symbol
    /// at a position, as a result to apply here, never applied by the
    /// server.
    pub fn rename(&self, uri: &str, position: Position, new_name: &str) -> Result<Value> {
        self.query(
            "textDocument/rename",
            json!({ "textDocument": { "uri": uri }, "position": position, "newName": new_name }),
        )
    }

    /// `textDocument/documentSymbol` for a document, or `None` when the
    /// server did not advertise the capability. The raw result is read by
    /// [`crate::lsp::outline::innermost_at`].
    pub fn document_symbol(&self, uri: &str) -> Result<Option<Value>> {
        if !self.supports("documentSymbolProvider") {
            return Ok(None);
        }
        self.query(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
        .map(Some)
    }
}
