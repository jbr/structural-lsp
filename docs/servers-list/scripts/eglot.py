from common import *
forms = dump_forms("../eglot/eglot.el", "defvar")
val = next(f[2] for f in forms if sym(f[1]) == "eglot-server-programs")
assert sym(val[0]) == "`"
entries = val[1]
records = []
def parse_modes(m):
    if S(m): return [m["s"]], {}
    modes, langids = [], {}
    for e in m:
        if S(e): modes.append(e["s"])
        else:
            modes.append(e[0]["s"])
            if len(e) >= 3 and sym(e[1]) == ":language-id": langids[e[0]["s"]] = e[2]
    return modes, langids
def split_cmd(lst):
    cmd, plist, i = [], {}, 0
    while i < len(lst):
        e = lst[i]
        if isinstance(e, str): cmd.append(e); i += 1
        elif S(e) and e["s"].startswith(":"): plist[e["s"]] = lisp(lst[i+1]) if i+1 < len(lst) else None; i += 2
        else: cmd.append(lisp(e)); i += 1
    return cmd, plist
for order, ent in enumerate(entries):
    modes, langids = parse_modes(ent[0])
    rest = ent[1:]
    base = dict(major_modes=modes, language_ids=langids or None, order_in_table=order,
                root={"strategy": "project.el project-current (VC root by default); eglot-lsp-context bound so project-find-functions can special-case LSP; else default-directory"})
    if rest and sym(rest[0]) == ",":
        inner = rest[1]
        if isinstance(inner, list) and sym(inner[0]) == "eglot-alternatives":
            alts = inner[1][1]
            for ai, a in enumerate(alts):
                cmd = [a] if isinstance(a, str) else split_cmd(a)[0]
                records.append(rec("eglot", cmd[0], command=cmd, executables=[cmd[0].split()[0]],
                    selection={"strategy": "eglot-alternatives: first executable-find hit wins (prompt if interactive and >1)", "alternative_index": ai, "alternatives_count": len(alts)},
                    **base))
        else:
            records.append(rec("eglot", None, command=None, command_raw=lisp(inner),
                selection={"strategy": "custom contact function (computed at startup)"}, **base))
    else:
        cls = None
        if rest and S(rest[0]) and not rest[0]["s"].startswith(":"):
            cls = rest[0]["s"]; rest = rest[1:]
        cmd, plist = split_cmd(rest)
        is_port = any(isinstance(e, int) for e in rest)
        records.append(rec("eglot", cmd[0] if cmd else None, command=cmd, executables=[cmd[0]] if cmd else [],
            server_class=cls, initialization=plist or None,
            selection={"strategy": "single fixed contact"}, **base))
json.dump(records, open("out/eglot.json", "w"), indent=1)
print(len(entries), "entries ->", len(records), "records")
