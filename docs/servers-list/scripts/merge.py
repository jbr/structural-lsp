import json, re, os, csv, collections

O = "out"
L = lambda f: json.load(open(f"{O}/{f}"))
FT = L("filetypes.json")
NV = FT["neovim_filetype"]["map"]
KSH = FT["kate_highlighting_mode"]["map"]
KAKFT = FT["kakoune_filetype"]["map"]
ZL = FT["zed_language"]["map"]
EMX = FT["emacs_extension_test"]["map"]
helix = L("helix.json")
HXL = {l["language"]: l for l in helix["languages"]}

# ---------- helpers ----------
def uniq(xs):
    seen, out = set(), []
    for x in xs:
        if x is None: continue
        k = json.dumps(x, sort_keys=True) if not isinstance(x, str) else x
        if k not in seen: seen.add(k); out.append(x)
    return out

def exe_norm(x):
    if not x or not isinstance(x, str): return None
    x = x.strip()
    if x.startswith("<") or " " in x.strip() and not x.endswith(")"): 
        x = x.split()[0] if not x.startswith("<") else x
    x = re.sub(r"\s*\(.*\)$", "", x)
    b = os.path.basename(x)
    b = re.sub(r"\.(exe|cmd|bat)$", "", b)
    return b or None

LOOKS_BIN = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.+-]*$")
def plausible_bins(strings):
    out = []
    for s in strings or []:
        if isinstance(s, str) and LOOKS_BIN.match(s) and not s.startswith("-") and len(s) < 45 and not re.fullmatch(r"[\d.]+", s):
            out.append(s)
    return out

EMX_BY_MODE = collections.defaultdict(list)
for e, v in EMX.items():
    if v["auto_mode_alist_mode"]: EMX_BY_MODE[v["auto_mode_alist_mode"]].append(e)
LSPMODE_LANG_EXT = collections.defaultdict(list)
for e, v in EMX.items():
    if v["lsp_mode_language_id_regexp"]: LSPMODE_LANG_EXT[v["lsp_mode_language_id_regexp"]].append(e)
LSPMODE_LANGCFG = L("lsp-mode-language-ids.json")
MODE_TO_LSPLANG = {x["match"]: x["language_id"] for x in LSPMODE_LANGCFG if x["match_kind"] == "major-mode"}

def ext_for_name(name):
    """heuristic: look a language/filetype name up in other editors' tables"""
    n = name.lower()
    cands = [n, n.replace("-", ""), n.replace("_", "")]
    for c in cands:
        if c in NV and NV[c]["extensions"]: return sorted(NV[c]["extensions"]), f"neovim filetype '{c}'"
    for k, v in HXL.items():
        if k.lower() in cands:
            fts = [x for x in (v["file_types"] or []) if isinstance(x, str)]
            if fts: return sorted(fts), f"helix language '{k}'"
    for k, v in ZL.items():
        if k.lower() in cands and v.get("path_suffixes"): return sorted(v["path_suffixes"]), f"zed language '{k}'"
    return [], None

def ext_for_emacs_mode(mode):
    if mode in EMX_BY_MODE: return sorted(EMX_BY_MODE[mode]), "emacs auto-mode-alist"
    if mode in MODE_TO_LSPLANG and MODE_TO_LSPLANG[mode] in LSPMODE_LANG_EXT:
        return sorted(LSPMODE_LANG_EXT[MODE_TO_LSPLANG[mode]]), "lsp-mode language-id regexp"
    fam = re.sub(r"(-ts)?-mode$", "", mode)
    fam = re.sub(r"^(js2|rjsx)$", "javascript", fam)
    fam = {"c++": "cpp", "js": "javascript", "sh": "sh", "tuareg": "ocaml", "caml": "ocaml", "rustic": "rust", "objc": "objc", "enh-ruby": "ruby", "cperl": "perl", "latex": "tex", "LaTeX": "tex", "gfm": "markdown", "yaml": "yaml"}.get(fam, fam)
    exts, via = ext_for_name(fam)
    return exts, (f"heuristic: mode family '{fam}' -> {via}" if via else None)

def ext_for_nvim_ft(ft):
    v = NV.get(ft)
    return (sorted(v["extensions"]), sorted(v["filenames"])) if v else ([], [])

def mk(source, server_id, **kw):
    r = {"source": source, "server_id": server_id, "languages": [], "language_key_kind": None,
         "file_extensions": [], "file_extensions_via": None, "filenames": [],
         "command": None, "command_raw": None, "executables": [], "executables_confidence": "exact",
         "root_markers": None, "root_markers_semantics": None, "root_conditions_raw": None,
         "activation_conditions_raw": None, "default_enabled": None, "selection": None,
         "auto_install": None, "notes": []}
    r.update(kw)
    return r

R = []

# ---------- eglot ----------
for x in L("eglot.json"):
    exts, vias = [], set()
    for m in x["major_modes"]:
        e, v = ext_for_emacs_mode(m); exts += e; vias.add(v) if v else None
    cmd = x.get("command")
    exes = [exe_norm(cmd[0])] if cmd else []
    notes = []
    if cmd and " " in cmd[0]: notes.append(f"program string contains a space ({cmd[0]!r}); executable-find will not resolve it")
    raw = x.get("command_raw")
    if raw and not exes:
        exes = plausible_bins(re.findall(r'"([^"]+)"', raw)); 
    R.append(mk("eglot", x["server_id"] or (exes[0] if exes else None), languages=x["major_modes"], language_key_kind="emacs-major-mode",
        file_extensions=sorted(set(exts)), file_extensions_via="; ".join(sorted(vias)) or None,
        command=cmd, command_raw=raw, executables=uniq(exes), executables_confidence="exact" if cmd else "heuristic",
        root_markers=None, root_markers_semantics=x["root"]["strategy"],
        default_enabled=True, selection={**x["selection"], "table_order": x["order_in_table"]},
        extra={k: x.get(k) for k in ("language_ids", "server_class", "initialization") if x.get(k)}, notes=notes))

# ---------- lsp-mode ----------
for x in L("lsp-mode.json"):
    bins = []
    for vname, v in (x.get("resolved_variables") or {}).items():
        if not re.search(r"(command|server|executable|exe|path|bin|program|cmd)", vname) or re.search(r"args|option|flags", vname): continue
        if isinstance(v, str): bins.append(v)
        elif isinstance(v, list) and v and not v[0].startswith("-"): bins.append(v[0])
    for d in x.get("dependencies") or []:
        for de in d.get("definitions") or []:
            m = re.search(r":system\s+\"([^\"]+)\"", de); bins += [m.group(1)] if m else []
            m = re.search(r":npm\s.*:path\s+\"([^\"]+)\"", de); bins += [m.group(1)] if m else []
    bins += x.get("dependency_binaries") or []
    for fr in (x.get("called_functions") or {}).values(): bins += fr.get("executable_find") or []
    if x.get("strings_in_connection"): bins += [b for b in plausible_bins(x["strings_in_connection"][:1]) if b not in {"UTF8","npm","node","stdio","start","serve","server","exec"}]
    STOP = {"UTF8","extension","npm","node_modules","start","serve","stdio","server","Normal","exec","typecheck","up","warn","language-server","plugins","bin","composer","lsp","run","--stdio"}
    if not bins:
        for fr in (x.get("called_functions") or {}).values():
            bins += [b for b in plausible_bins(fr["strings"][:3]) if b not in STOP and not re.search(r"\d+\.\d+|\.(gz|zip|phar|tar)$", b)]
    if x.get("strings_in_connection") and not bins: pass
    bins = [b for b in bins if not re.search(r"\.(jar|js|py|json|dll|phar)$|%s|^\d|/$", b) and b not in {"UTF8","npm","node_modules","server","serve","Normal","warn","composer","stdio","start","npm","node"}]
    exts, via = [], None
    if isinstance(x.get("major_modes"), list):
        vs = set()
        for m in x["major_modes"]:
            e, v = ext_for_emacs_mode(m); exts += e; vs.add(v) if v else None
        via = "; ".join(sorted(vs)) or None
    langs = x["major_modes"] if isinstance(x.get("major_modes"), list) else (x.get("activate_on_languages") or [])
    kind = "emacs-major-mode" if isinstance(x.get("major_modes"), list) else ("lsp-mode-language-id" if x.get("activate_on_languages") else None)
    if kind == "lsp-mode-language-id":
        for lang in langs:
            e = LSPMODE_LANG_EXT.get(lang) or []
            if not e: e, _ = ext_for_name(lang)
            exts += e
        via = "lsp-mode language-id regexps / heuristic name match"
    R.append(mk("lsp-mode", x["server_id"], languages=langs, language_key_kind=kind,
        file_extensions=sorted(set(exts)), file_extensions_via=via,
        command_raw=x.get("connection_raw"), executables=uniq(exe_norm(b) for b in bins), executables_confidence="heuristic (resolved from variables, lsp-dependency and helper strings)",
        root_markers_semantics=x["root"]["strategy"],
        activation_conditions_raw=x.get("activation_fn_raw"),
        default_enabled=True, selection={**x["selection"], "priority": x["priority"], "add_on": x["add_on"]},
        extra={"package": x.get("package"), "file": x.get("file"), "remote": x["remote"], "multi_root": x["multi_root"], "dependencies": x.get("dependencies")}))

# ---------- lsp-bridge ----------
lb = L("lsp-bridge.json")
VAR_ROWS = collections.defaultdict(list)
for tname, rows in lb["mappings"].items():
    for row in rows:
        if row.get("via_variable"): VAR_ROWS[row["via_variable"]].append({"table": tname, **row})
for x in lb["records"]:
    langs, exts, kinds = [], [], set()
    default_for = []
    for var in x.get("listed_as_choice_in") or []:
        for row in VAR_ROWS.get(var, []):
            if row["server"] == x["server_id"]: continue
            langs += row["match"]; kinds.add(row["match_kind"] + " (as alternative)")
            for m in row["match"]:
                e, _ = (row["match"], None) if row["match_kind"] == "extension" else ext_for_emacs_mode(m); exts += e if row["match_kind"] != "extension" else [m]
    for ref in x.get("referenced_by") or []:
        if ref["table"] == "member_of_multiserver":
            default_for.append(f"multiserver:{ref['multiserver']}"); continue
        langs += ref["match"]; kinds.add(ref["match_kind"])
        if ref["match_kind"] == "extension": exts += ref["match"]
        else:
            for m in ref["match"]:
                e, _ = ext_for_emacs_mode(m); exts += e
        default_for.append(ref["table"] + (f" via {ref['via_variable']}" if ref.get("via_variable") else ""))
    R.append(mk("lsp-bridge", x["server_id"], languages=uniq(langs), language_key_kind="/".join(sorted(kinds)) or None,
        file_extensions=sorted(set(exts)), file_extensions_via="lsp-bridge extension tables + emacs mode heuristics" if exts else None,
        command=x["command"], executables=uniq(exe_norm(e) for e in x["executables"]),
        root_markers=x.get("project_files"), root_markers_semantics=x["root"]["strategy"],
        default_enabled=bool(default_for) and not all(d.startswith("multiserver") for d in default_for) or None,
        selection={**x["selection"], "referenced_by": default_for or None, "listed_as_choice_in": x.get("listed_as_choice_in")},
        extra={"file": x["file"], "platform": x["platform"], "language_id": x["language_id"], "support_single_file": x["support_single_file"]}))

# ---------- nvim-lspconfig ----------
mason = L("mason-registry.json")
MASON_BY_LSPCONFIG = {}
for m in mason:
    if m.get("lspconfig_name"): MASON_BY_LSPCONFIG.setdefault(m["lspconfig_name"], []).append(m)
for x in json.load(open(f"{O}/nvim-lspconfig.raw.json")):
    cmd = x.get("command")
    exes, conf = ([exe_norm(cmd[0])] if cmd else []), "exact"
    if not cmd and x.get("command_fn_source"):
        exes = plausible_bins(re.findall(r"'([^']+)'", x["command_fn_source"]))[:3]; conf = "heuristic (string literals in cmd function)"
    fts = x.get("filetypes") or []
    exts, fns, hvia = [], [], set()
    for ft in fts:
        e, f = ext_for_nvim_ft(ft)
        if not e and not f:
            e, v = ext_for_name(ft.split(".")[0])
            if v: hvia.add(v)
        exts += e; fns += f
    markers = None
    if x.get("root_markers"):
        markers = []
        for m in x["root_markers"]:
            markers += m if isinstance(m, list) else [m]
    rfn = x.get("root_dir_fn_source")
    mm = MASON_BY_LSPCONFIG.get(x["server_id"], [])
    R.append(mk("nvim-lspconfig", x["server_id"], languages=fts, language_key_kind="vim-filetype" if fts else "any filetype (no filetypes set)",
        file_extensions=sorted(set(exts)), filenames=sorted(set(fns)), file_extensions_via=("neovim vim.filetype tables" + ("; heuristic fallback: " + "; ".join(sorted(hvia)) if hvia else "")) if exts else None,
        command=cmd, command_raw=x.get("command_fn_source"), executables=uniq(exes), executables_confidence=conf,
        root_markers=markers, root_markers_semantics="root_markers: list order = priority, nested list = equal priority, nearest ancestor per marker (vim.fs.root); root_dir function (if present) decides and may decline" if (markers or rfn) else "no root: cwd/single-file",
        root_conditions_raw=rfn,
        default_enabled=False, selection={"strategy": "opt-in via vim.lsp.enable(name) (or mason-lspconfig automatic_enable for installed servers); every enabled config whose filetype matches, cmd is executable, and root_dir resolves is started — no mutual exclusion"},
        auto_install={"mason_packages": [{"name": p["server_id"], "purl": p["purl"], "bin": p["executables"]} for p in mm]} if mm else None,
        extra={"homepage": x.get("homepage"), "workspace_required": x.get("workspace_required"), "file": x["file"]}))

# ---------- helix ----------
for x in helix["records"]:
    langs = [u["language"] for u in x["default_in_languages"]]
    exts, fns, roots = [], [], []
    for u in x["default_in_languages"]:
        for ft in u["file_types"] or []:
            if isinstance(ft, str): exts.append(ft)
            elif isinstance(ft, dict) and "glob" in ft: fns.append(ft["glob"])
        roots += u["roots"] or []
    R.append(mk("helix", x["server_id"], languages=langs, language_key_kind="helix-language",
        file_extensions=sorted(set(exts)), filenames=sorted(set(fns)), file_extensions_via="helix languages.toml file-types" if exts else None,
        command=x["command"], executables=uniq(exe_norm(e) for e in x["executables"]),
        root_markers=sorted(set(roots)) or None, root_markers_semantics=x["root"]["strategy"],
        root_conditions_raw=json.dumps({"required-root-patterns": x["required_root_patterns"]}) if x["required_root_patterns"] else None,
        default_enabled=bool(langs), selection={**x["selection"], "positions": [{"language": u["language"], "position": u["position"], "of": u["of"], "features": u["features"]} for u in x["default_in_languages"]]},
        extra={"shebangs": sorted({s for u in x["default_in_languages"] for s in (u["shebangs"] or [])}) or None}))

# ---------- vim-lsp-settings ----------
for x in L("vim-lsp-settings.json"):
    e, f = ext_for_nvim_ft(x["filetype"])
    conds = "\n\n".join(filter(None, [x.get("blocklist_fn_source"), x.get("fallback_checker") and "checker:\n" + x["fallback_checker"]]))
    R.append(mk("vim-lsp-settings", x["server_id"], languages=[x["filetype"]], language_key_kind="vim-filetype",
        file_extensions=e, filenames=f, file_extensions_via="neovim vim.filetype tables (proxy for Vim filetype detection)" if e else None,
        executables=uniq(exe_norm(v) for v in x["executables"]),
        root_markers=x.get("root_uri_patterns"), root_markers_semantics=x["root"]["strategy"],
        activation_conditions_raw=conds or None,
        default_enabled=not x["disabled_by_default"],
        selection={**x["selection"], "order_in_filetype": x["order_in_filetype"], "requires": x["requires"]},
        extra={"url": x["url"], "description": x["description"], "allowlist_raw": x["allowlist_raw"], "blocklist_raw": x["blocklist_raw"]}))

# ---------- kakoune-lsp ----------
def kak_exts(ft_regex):
    body = ft_regex.strip("()").replace("?:", "")
    names = [n for n in re.split(r"[|()]", body) if n]
    exts = []
    for n in names:
        exts += KAKFT.get(n, {}).get("extensions", [])
    return names, sorted(set(exts))
for x in L("kakoune-lsp.json"):
    names, exts = kak_exts(x["filetype_regex"])
    R.append(mk("kakoune-lsp", x["server_id"], languages=names, language_key_kind="kakoune-filetype",
        file_extensions=exts, file_extensions_via="kakoune BufCreate regexes tested against candidate extensions" if exts else None,
        command=x["command"], executables=uniq(exe_norm(e) for e in x["executables"]),
        root_markers=x.get("root_globs"), root_markers_semantics=x["root_strategy"],
        default_enabled=x["enabled_by_default"], selection=x["selection"],
        extra={"filetype_regex": x["filetype_regex"], "single_instance": x.get("single_instance")}))

# ---------- kate ----------
for x in L("kate.json"):
    rx = x.get("highlighting_mode_regex")
    modes, exts = [], []
    if rx:
        try:
            for name, v in KSH.items():
                if re.search(rx, name): modes.append(name); exts += [e[2:] if e.startswith("*.") else e for e in v["extensions"]]
        except re.error: pass
    markers = (x.get("root_indication_file_names") or []) + (x.get("root_indication_file_patterns") or [])
    R.append(mk("kate", x["server_id"], languages=modes, language_key_kind="ksyntaxhighlighting-mode",
        file_extensions=sorted(set(exts)), file_extensions_via="KSyntaxHighlighting definitions matched by highlightingModeRegex" if exts else None,
        command=x["command"] if isinstance(x["command"], list) else None, command_raw=x["command"] if isinstance(x["command"], str) else None,
        executables=uniq(exe_norm(e) for e in x["executables"]),
        root_markers=markers or None, root_markers_semantics=x["root"]["strategy"],
        default_enabled=bool(x["command"]), selection=x["selection"],
        notes=[x["note"]] if x.get("note") else [],
        extra={"highlighting_mode_regex": rx, "uses": x.get("uses"), "root_setting": x.get("root_setting"), "extra_search_paths": x.get("extra_search_paths"), "url": x.get("url")}))

# ---------- opencode ----------
for x in json.load(open(f"{O}/opencode.raw.json")):
    whiches = sorted({a for c in x.get("spawn_calls", []) if c["callee"] == "which" for a in c["args"] if a})
    npm = sorted({a for c in x.get("spawn_calls", []) if c["callee"] == "Npm.which" for a in c["args"][:1] if a})
    npm_bins = sorted({(c["args"][1] if len(c["args"]) > 1 and c["args"][1] else c["args"][0].split("/")[-1]) for c in x.get("spawn_calls", []) if c["callee"] == "Npm.which" and c["args"] and c["args"][0]})
    fetches = sorted({a for c in x.get("spawn_calls", []) if c["callee"] == "fetch" for a in c["args"] if a})
    root = x.get("root", {})
    markers = root.get("include") or (root.get("string_literals") if root.get("helper") == "custom" else None)
    exts = [e.lstrip(".") for e in (x.get("extensions") or []) if e.startswith(".")]
    fns = [e for e in (x.get("extensions") or []) if not e.startswith(".")]
    R.append(mk("opencode", x["server_id"], languages=x.get("extensions") or [], language_key_kind="file-extension",
        file_extensions=exts, filenames=fns, file_extensions_via="opencode server extensions",
        executables=uniq(whiches + npm_bins), executables_confidence="which() arguments exact; Npm.which bin names (second arg, or package basename) included",
        root_markers=markers, root_markers_semantics=f"{root.get('helper')}: nearest ancestor containing any include marker, bounded by project dir; any exclude marker upward -> server skipped; NearestRoot falls back to project dir, StrictNearestRoot/custom may return undefined (skip)",
        root_conditions_raw=json.dumps({"exclude": root.get("exclude")}) if root.get("exclude") else (root.get("source") if root.get("helper") == "custom" else None),
        default_enabled=x["server_id"] != "ty",
        selection={"strategy": "every server whose extensions match and whose root() returns a path is spawned (one client per root+server); spawn returns nothing if binary unavailable", "experimental_flag": "experimentalLspTy swaps pyright->ty" if x["server_id"] in ("ty", "pyright") else None},
        auto_install={"npm": npm or None, "downloads": fetches or None} if (npm or fetches) else None))

# ---------- zed ----------
zed = L("zed.json")
ZDEF = zed["default_language_servers_setting"]
def zed_default(sid, langs):
    st = []
    for lang in langs:
        lst = ZDEF.get(lang, ZDEF.get("*"))
        if lst is None: st.append(True); continue
        if f"!{sid}" in lst: st.append(False)
        elif sid in lst or "..." in lst: st.append(True)
        else: st.append(False)
    return (any(st) if st else None)
for x in zed["records"]:
    exts, fns, miss = [], [], []
    for lang in x["languages"]:
        v = ZL.get(lang)
        if not v: miss.append(lang); continue
        for s in v.get("path_suffixes") or []:
            (exts if re.fullmatch(r"[\w+-]+", s) else fns).append(s)
    notes = [x["notes"]] if x.get("notes") else []
    if miss: notes.append(f"language names not defined by any fetched Zed language config: {miss}")
    if x["source"] == "zed-extension" and not x["executables_via_which"]:
        notes.append("extension never calls worktree.which(): server is always installed/downloaded (see auto_install) or launched via a runtime")
    R.append(mk(x["source"], x["server_id"], languages=x["languages"], language_key_kind="zed-language",
        file_extensions=sorted(set(exts)), filenames=sorted(set(fns)), file_extensions_via="zed language path_suffixes" if exts or fns else None,
        command=x.get("manifest_command"),
        executables=uniq(exe_norm(e) for e in x["executables_via_which"] if not e.startswith("<")),
        executables_confidence="heuristic (crate-wide which() calls with const resolution)" if x["source"] == "zed-extension" else "exact (hand-verified)",
        root_markers=x.get("worktree_files_read"), root_markers_semantics=x["root"]["strategy"] + ("; manifest provider: " + x["manifest_provider"] if x.get("manifest_provider") else ""),
        default_enabled=zed_default(x["server_id"], x["languages"]),
        selection=x["selection"], auto_install=x["auto_install"], notes=notes,
        extra={k: x.get(k) for k in ("extension_id", "extension_version", "repository", "display_name", "language_ids", "string_consts") if x.get(k)} or None))

# ---------- mason (supplementary) ----------
for m in mason:
    if "LSP" not in (m["categories"] or []): continue
    exts = []
    for lang in m["languages"] or []:
        e, _ = ext_for_name(lang); exts += e
    R.append(mk("mason-registry", m["server_id"], languages=m["languages"] or [], language_key_kind="mason-language-name",
        file_extensions=sorted(set(exts)), file_extensions_via="heuristic name match" if exts else None,
        executables=m["executables"], default_enabled=None,
        selection={"strategy": "installer registry (not a selector); nvim-lspconfig name: " + str(m["lspconfig_name"])},
        auto_install={"purl": m["purl"]}, extra={"homepage": m["homepage"], "deprecation": m["deprecation"], "lspconfig_name": m["lspconfig_name"]}))

# ---------- finalize ----------
for r in R:
    r["executables"] = [e for e in uniq(r["executables"]) if e]
    if not r.get("extra"): r.pop("extra", None)
    if not r["notes"]: r.pop("notes")
    r["id"] = f"{r['source']}:{r['server_id']}"
ids = collections.Counter(r["id"] for r in R)
seen = collections.Counter()
for r in R:
    if ids[r["id"]] > 1:
        seen[r["id"]] += 1; r["id"] += f"#{seen[r['id']]}"
json.dump(R, open(f"{O}/../dist/servers.json", "w"), indent=1, ensure_ascii=False)

# by executable
BX = collections.defaultdict(list)
for r in R:
    for e in r["executables"]:
        BX[e].append({"id": r["id"], "source": r["source"], "server_id": r["server_id"], "languages": r["languages"][:12],
                      "file_extensions": r["file_extensions"][:30], "root_markers": r["root_markers"], "default_enabled": r["default_enabled"],
                      "has_conditions": bool(r.get("root_conditions_raw") or r.get("activation_conditions_raw"))})
bx = {e: {"sources": sorted({v["source"] for v in vs}), "count": len(vs), "entries": vs} for e, vs in sorted(BX.items(), key=lambda kv: (-len({v['source'] for v in kv[1]}), kv[0]))}
json.dump(bx, open(f"{O}/../dist/by_executable.json", "w"), indent=1, ensure_ascii=False)

# by extension
BE = collections.defaultdict(lambda: collections.defaultdict(set))
for r in R:
    if r["source"] == "mason-registry": continue
    for e in r["file_extensions"]:
        for x in (r["executables"] or [f"<{r['server_id']}>"]):
            BE[e.lower()][x].add(r["source"])
be = {e: {x: sorted(s) for x, s in sorted(d.items(), key=lambda kv: -len(kv[1]))} for e, d in sorted(BE.items())}
json.dump(be, open(f"{O}/../dist/by_extension.json", "w"), indent=1, ensure_ascii=False)

# CSV
with open(f"{O}/../dist/servers.csv", "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["id", "source", "server_id", "executables", "executables_confidence", "languages", "file_extensions", "root_markers", "has_root_or_activation_logic", "default_enabled"])
    for r in R:
        w.writerow([r["id"], r["source"], r["server_id"], " ".join(r["executables"]), r["executables_confidence"], " ".join(map(str, r["languages"][:20])),
                    " ".join(r["file_extensions"][:40]), " ".join(r["root_markers"] or []), bool(r.get("root_conditions_raw") or r.get("activation_conditions_raw")), r["default_enabled"]])

stats = collections.Counter(r["source"] for r in R)
print(dict(stats), "total", len(R))
print("distinct executables:", len(bx), "; seen in >=5 sources:", sum(1 for v in bx.values() if len(v["sources"]) >= 5))
print("records without executables:", collections.Counter(r["source"] for r in R if not r["executables"]))
print("records without extensions:", collections.Counter(r["source"] for r in R if not r["file_extensions"] and not r["filenames"]))
