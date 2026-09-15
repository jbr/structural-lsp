import json, os, re, glob
base = "../vim-lsp-settings"
d = json.load(open(f"{base}/settings.json")); d.pop("$schema", None)
def vimfile(cmd):
    p = f"{base}/settings/{cmd}.vim"
    return open(p).read() if os.path.exists(p) else None
def funcs(txt):
    return {m.group(1): m.group(0) for m in re.finditer(r"function! (s:\w+)\(.*?\n(?:.*\n)*?endfunction", txt)} if txt else {}
records = []
seen = {}
for ft, entries in d.items():
    for i, e in enumerate(entries):
        cmd = e["command"]
        txt = vimfile(cmd)
        allow = re.search(r"'allowlist':\s*lsp_settings#get\('[^']+',\s*'allowlist',\s*(.+?)\),?\s*\n", txt or "")
        block = re.search(r"'blocklist':\s*lsp_settings#get\('[^']+',\s*'blocklist',\s*(.+?)\),?\s*\n", txt or "")
        exe = re.findall(r"lsp_settings#exec_path\('([^']+)'\)", txt or "")
        fn = funcs(txt)
        checker = f"{base}/checkers/{cmd}.vim"
        records.append({"source": "vim-lsp-settings", "server_id": cmd, "filetype": ft, "order_in_filetype": i,
            "requires": e.get("requires"), "root_uri_patterns": e.get("root_uri_patterns"),
            "disabled_by_default": e.get("disabled", False), "vim_plugin": e.get("vim_plugin"),
            "url": e.get("url"), "description": e.get("description"),
            "executables": sorted(set(exe)) or [cmd],
            "allowlist_raw": allow.group(1).strip() if allow else None,
            "blocklist_raw": block.group(1).strip() if block else None,
            "blocklist_fn_source": fn.get("s:get_blocklist"),
            "other_local_functions": sorted(k for k in fn if k != "s:get_blocklist") or None,
            "fallback_checker": open(checker).read() if os.path.exists(checker) else None,
            "settings_file_present": txt is not None,
            "selection": {"strategy": "walk filetype list in order; first entry not disabled, with all `requires` executable and command found (PATH, g:lsp_settings_extra_paths, own servers dir, or checker) is registered and loop stops; g:lsp_settings_filetype_<ft> as list registers several"},
            "root": {"strategy": "nearest ancestor containing any of root_uri_patterns + g:lsp_settings_root_markers (.git .git/ .svn/ .hg/ .bzr/)", "markers": e.get("root_uri_patterns")}})
json.dump(records, open("out/vim-lsp-settings.json", "w"), indent=1)
print(len(records), "entries;", len({r['server_id'] for r in records}), "distinct servers;", len(d), "filetypes;", sum(1 for r in records if r['blocklist_fn_source']), "with blocklist fns")
