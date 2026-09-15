import tomllib, json
d = tomllib.load(open("../helix-languages.toml", "rb"))
ls = d.get("language-server", {})
langs = d.get("language", [])
use = {}
for L in langs:
    for i, s in enumerate(L.get("language-servers", [])):
        n = s if isinstance(s, str) else s["name"]
        feat = None if isinstance(s, str) else {k: v for k, v in s.items() if k != "name"}
        use.setdefault(n, []).append({"language": L["name"], "position": i, "of": len(L["language-servers"]), "features": feat,
            "file_types": L.get("file-types"), "shebangs": L.get("shebangs"), "roots": L.get("roots")})
records = []
for name, cfg in ls.items():
    records.append({"source": "helix", "server_id": name,
        "command": [cfg.get("command")] + cfg.get("args", []) if cfg.get("command") else None,
        "executables": [cfg["command"]] if cfg.get("command") else [],
        "required_root_patterns": cfg.get("required-root-patterns"),
        "environment": cfg.get("environment"),
        "default_in_languages": use.get(name, []),
        "selection": {"strategy": "all servers in a language's language-servers list start; per-feature first server in list order wins; required-root-patterns gates startup"},
        "root": {"strategy": "workspace = first .git/.svn/.jj/.helix above cwd; LSP root = TOPMOST dir between file and workspace containing a language `roots` marker (per language, not per server); workspace-lsp-roots can cap search"}})
langrecs = [{"language": L["name"], "file_types": L.get("file-types"), "shebangs": L.get("shebangs"), "roots": L.get("roots"),
             "language_servers": L.get("language-servers"), "language_id": L.get("language-id")} for L in langs]
json.dump({"records": records, "languages": langrecs, "undefined_server_refs": sorted(set(use) - set(ls))}, open("out/helix.json", "w"), indent=1)
print(len(records), "servers", len(langrecs), "languages; unused servers:", sum(1 for r in records if not r["default_in_languages"]), "; undefined refs:", sorted(set(use) - set(ls)))
