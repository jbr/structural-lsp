import tomllib, glob, os, re, json
Z = "cache/zsrc"
def rs_files(d): return glob.glob(f"{d}/**/*.rs", recursive=True)
def resolve_expr(expr, consts):
    e = expr.strip().lstrip("&").strip()
    e = re.sub(r"\.(as_ref|as_str|to_string|into|as_os_str)\(\)$", "", e)
    m = re.fullmatch(r'"([^"]*)"', e)
    if m: return m.group(1)
    key = e.split("::")[-1]
    return consts.get(key)
def scan(d):
    text = "\n".join(open(f, errors="ignore").read() for f in rs_files(d))
    consts = dict(re.findall(r'const\s+([A-Z_][A-Z0-9_]*)\s*:\s*&(?:\'static\s+)?str\s*=\s*"([^"]*)"', text))
    def calls(fn):
        out = []
        for m in re.finditer(fn + r"\(\s*([^()]*?(?:\([^()]*\))?[^()]*?)\s*[,)]", text):
            arg = m.group(1); out.append({"expr": arg.strip(), "resolved": resolve_expr(arg, consts)})
        return out
    uniq = lambda xs: [dict(t) for t in {tuple(sorted(x.items())) for x in xs}]
    return {
        "which": uniq(calls(r"\.which")),
        "github_releases": uniq(calls(r"latest_github_release") + calls(r"zed::github_release_by_tag_name")),
        "npm_packages": uniq(calls(r"npm_install_package") + calls(r"npm_package_latest_version")),
        "worktree_files_read": sorted(set(r for r in (resolve_expr(a, consts) for a in re.findall(r"read_text_file\(\s*([^)]*?)\s*\)", text)) if r)),
        "string_consts": {k: v for k, v in consts.items() if re.search(r"BIN|SERVER|EXEC|COMMAND|PACKAGE|NAME|REPO|LSP", k)},
        "uses_shell_env": "shell_env" in text,
        "reads_lsp_settings_binary_path": bool(re.search(r"LspSettings::for_worktree", text)),
    }
langs = {}   # name -> {suffixes, first_line, source}
records = []
# ---- built-in languages
for cfg in glob.glob("../zed/crates/grammars/src/*/config.toml"):
    d = tomllib.load(open(cfg, "rb"))
    langs[d["name"]] = {"path_suffixes": d.get("path_suffixes"), "first_line_pattern": d.get("first_line_pattern"), "provided_by": "zed built-in"}
# ---- extension languages + servers
for n in sorted(os.listdir(Z)):
    d = f"{Z}/{n}"
    try: man = tomllib.load(open(f"{d}/extension.toml", "rb"))
    except Exception: continue
    for cfg in glob.glob(f"{d}/languages/*/config.toml"):
        try: c = tomllib.load(open(cfg, "rb"))
        except Exception: continue
        if "name" in c:
            langs.setdefault(c["name"], {"path_suffixes": c.get("path_suffixes"), "first_line_pattern": c.get("first_line_pattern"), "provided_by": f"extension:{n}"})
    sc = scan(d)
    for sid, s in (man.get("language_servers") or {}).items():
        L = s.get("languages") or ([s["language"]] if s.get("language") else [])
        records.append({"source": "zed-extension", "server_id": sid, "extension_id": n, "extension_version": man.get("version"),
            "repository": man.get("repository"), "display_name": s.get("name"), "languages": L, "language_ids": s.get("language_ids"),
            "manifest_command": ([s["command"]] + s.get("args", [])) if s.get("command") else None,
            "executables_via_which": sorted({(w["resolved"] or (sid if re.search(r"language_server_id|\bid\.as_ref|\bid\b", w["expr"]) else f"<{w['expr']}>")) for w in sc["which"]}),
            "auto_install": {"github_releases": sorted({g["resolved"] or f"<{g['expr']}>" for g in sc["github_releases"]}),
                             "npm_packages": sorted({p["resolved"] or f"<{p['expr']}>" for p in sc["npm_packages"]})},
            "worktree_files_read": sc["worktree_files_read"] or None,
            "string_consts": sc["string_consts"] or None,
            "notes": "crate-level scan: which()/install calls are attributed to every server in the extension" if len(man["language_servers"]) > 1 else None,
            "selection": {"strategy": "extension registers adapter for listed languages; enabled per languages.<L>.language_servers (\"...\" expands, \"!x\" excludes); extension code typically checks lsp.<id>.binary.path setting, then worktree.which(), then downloads"},
            "root": {"strategy": "Zed worktree root (opened folder); extension may inspect worktree files; no per-server marker search"}})
# ---- built-in adapters (hand-encoded from crates/languages/src, verified by grep)
B = [("bash-language-server", ["Shell Script"], ["bash-language-server"], [], ["bash-language-server"]),
     ("clangd", ["C", "C++"], ["clangd"], ["clangd/clangd"], []),
     ("vscode-css-language-server", ["CSS"], ["vscode-css-language-server"], [], ["vscode-langservers-extracted"]),
     ("eslint", ["TSX", "TypeScript", "JavaScript", "Vue.js", "Svelte"], [], ["microsoft/vscode-eslint (source build)"], []),
     ("gopls", ["Go", "Go Mod", "Go Work"], ["gopls", "go"], ["golang/tools (version check; installs via go install)"], []),
     ("json-language-server", ["JSON", "JSONC"], ["vscode-json-language-server"], [], ["vscode-langservers-extracted"]),
     ("package-version-server", ["JSON"], ["package-version-server"], ["zed-industries/package-version-server"], []),
     ("basedpyright", ["Python"], ["basedpyright-langserver", "node"], [], ["basedpyright"]),
     ("ruff", ["Python"], ["ruff (also venv bin)"], ["astral-sh/ruff"], []),
     ("ty", ["Python"], ["ty (also venv bin)"], ["astral-sh/ty"], []),
     ("pylsp", ["Python"], ["pylsp (also venv bin)"], ["pip install into Zed-managed venv"], []),
     ("pyright", ["Python"], ["pyright-langserver", "node"], [], ["pyright"]),
     ("rust-analyzer", ["Rust"], ["rust-analyzer", "rustup (rustup which rust-analyzer)"], ["rust-lang/rust-analyzer"], []),
     ("tailwindcss-language-server", ["Astro", "CSS", "ERB", "HTML+ERB", "HEEx", "HTML", "JavaScript", "TypeScript", "PHP", "Svelte", "TSX", "Vue.js"], ["tailwindcss-language-server"], [], ["@tailwindcss/language-server"]),
     ("tailwindcss-intellisense-css", [], ["tailwindcss-intellisense-css"], [], ["@tailwindcss/language-server"]),
     ("typescript-language-server", ["TypeScript", "TSX", "JavaScript", "JSDoc"], [], [], ["typescript-language-server", "typescript"]),
     ("vtsls", ["TypeScript", "TSX", "JavaScript", "JSDoc"], ["vtsls"], [], ["@vtsls/language-server"]),
     ("yaml-language-server", ["YAML"], ["yaml-language-server"], [], ["yaml-language-server"])]
for sid, L, w, gh, npm in B:
    records.append({"source": "zed-builtin", "server_id": sid, "languages": L, "executables_via_which": w,
        "auto_install": {"github_releases": gh, "npm_packages": npm},
        "manifest_provider": {"Python": "pyproject.toml (innermost; outermost with uv.lock/poetry.lock/pdm.lock/Pipfile.lock wins)", "Rust": "Cargo.toml"}.get(L[0]) if L else None,
        "selection": {"strategy": "which() on PATH unless lsp.<id>.binary.ignore_system_version, else download; enabled per language_servers setting"},
        "root": {"strategy": "worktree root, refined by ManifestProvider where registered"}})
dflt = json.load(open("cache/zed-default.clean.json"))
defaults = {"*": dflt.get("language_servers"), **{k: v["language_servers"] for k, v in dflt.get("languages", {}).items() if "language_servers" in v}}
json.dump({"records": records, "languages": langs, "default_language_servers_setting": defaults}, open("out/zed.json", "w"), indent=1)
print(len(records), "records;", len(langs), "languages;", len(defaults), "default lists;",
      sum(1 for r in records if r["source"] == "zed-extension" and not r["executables_via_which"]), "ext servers without which()")
