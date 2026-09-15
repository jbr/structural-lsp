import re, tomllib, json
txt = open("../kakoune-lsp/rc/servers.kak").read()
def match_block(s, i):
    # s[i] == '{' or '"' ; return end index (exclusive) of content
    if s[i] == '"':
        j = i + 1
        while j < len(s):
            if s[j] == '"' and s[j+1:j+2] != '"': return i+1, j
            if s[j] == '"' : j += 2; continue
            j += 1
    depth, j = 0, i
    while j < len(s):
        if s[j] == '{': depth += 1
        elif s[j] == '}':
            depth -= 1
            if depth == 0: return i+1, j
        j += 1
    raise ValueError
def blocks(body):
    res = []
    for m in re.finditer(r"set-option\s+(-add\s+)?buffer\s+lsp_servers\s+(%\{|\")", body):
        start = m.end() - 1
        a, b = match_block(body, start)
        res.append((bool(m.group(1)), body[a:b] if m.group(2) == "%{" else body[a:b].replace('""', '"')))
    return res
def parse_toml(t):
    try: return tomllib.loads(t), None
    except Exception as e: return None, str(e)
shared = {}
for dm in re.finditer(r"declare-option -hidden str (lsp_server_\w+) %\{", txt):
    a, b = match_block(txt, dm.end() - 1)
    shared[dm.group(1)] = txt[a:b]
def expand_opts(body):
    return re.sub(r"(set-option\s+(?:-add\s+)?buffer\s+lsp_servers\s+)%opt\{(lsp_server_\w+)\}",
                  lambda mm: mm.group(1) + "%{" + shared.get(mm.group(2), "") + "}", body)
records = []
for m in re.finditer(r"hook -group (\S+) global BufSetOption filetype=(\S+) %\{", txt):
    a, b = match_block(txt, m.end() - 1)
    body = expand_opts(txt[a:b])
    active_body = "\n".join(l for l in body.splitlines() if not l.lstrip().startswith("#"))
    out_lines, group = [], []
    def flush():
        g = "\n".join(group)
        if re.search(r"set-option\s+(-add\s+)?buffer\s+lsp_servers", g):
            out_lines.extend(re.sub(r"^(\s*)# ?", r"\1", l, count=1) for l in group)
        else:
            out_lines.extend(group)
        group.clear()
    for l in body.splitlines():
        if l.lstrip().startswith("#"): group.append(l)
        else: flush(); out_lines.append(l)
    flush()
    decommented = "\n".join(out_lines)
    active = {}
    for add, t in blocks(active_body):
        p, err = parse_toml(t)
        for k, v in (p or {}).items(): active[k] = (v, add)
    allsrv = {}
    for add, t in blocks(decommented):
        p, err = parse_toml(t)
        if err: allsrv.setdefault("__parse_error__", []).append(err)
        for k, v in (p or {}).items():
            if isinstance(v, dict): allsrv[k] = (v, add)
    ft = m.group(2)
    for k, val in allsrv.items():
        if k == "__parse_error__": continue
        v, add = val
        cmd = v.get("command", k)
        records.append({"source": "kakoune-lsp", "server_id": k, "hook_group": m.group(1), "filetype_regex": ft,
            "enabled_by_default": k in active, "added_with_-add": add,
            "command": [cmd] + v.get("args", []), "executables": [cmd],
            "root_globs": v.get("root_globs"), "root": v.get("root"), "single_instance": v.get("single_instance"),
            "selection": {"strategy": "filetype hook sets lsp_servers TOML; every active server in it runs; commented-out alternatives shown as enabled_by_default=false"},
            "root_strategy": "root_globs tried in LIST ORDER, each searched upward to /; first glob found anywhere above wins; KAK_LSP_PROJECT_ROOT_<LANG> / KAK_LSP_FORCE_PROJECT_ROOT override"})
    if "__parse_error__" in allsrv: print("parse error in", m.group(1), allsrv["__parse_error__"][:1])
json.dump(records, open("out/kakoune-lsp.json", "w"), indent=1)
print(len(records), "server entries;", sum(r["enabled_by_default"] for r in records), "active;", len({r['hook_group'] for r in records}), "hooks")
