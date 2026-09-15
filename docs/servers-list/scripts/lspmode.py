from common import *
import glob, re
from concurrent.futures import ThreadPoolExecutor
files = sorted(glob.glob("../lsp-mode/clients/*.el")) + ["../lsp-mode/lsp-mode.el"] + sorted(glob.glob("/home/claude/lspext/flat/*.el"))
def load(f): return f, dump_forms(f, "make-lsp-client", "make-lsp--client", "defcustom", "defvar", "lsp-dependency", "defconst", "defun", "lsp-defun", "cl-defun")
with ThreadPoolExecutor(1) as ex: loaded = list(ex.map(load, files))
# global variable defaults & dependencies
vars_, deps, funs = {}, {}, {}
for f, forms in loaded:
    for fm in forms:
        h = sym(fm[0])
        if h in ("defcustom", "defvar", "defconst") and len(fm) > 2 and S(fm[1]):
            vars_[fm[1]["s"]] = fm[2]
        elif h == "lsp-dependency" and len(fm) > 1:
            name = fm[1][1]["s"] if isinstance(fm[1], list) and S(fm[1][1]) else lisp(fm[1])
            deps[name] = fm[2:]
        elif h in ("defun", "lsp-defun", "cl-defun") and len(fm) > 3 and S(fm[1]):
            if not f.endswith("lsp-mode.el"):
                body = fm[3:]
                if body and isinstance(body[0], str): body = body[1:]
                funs[fm[1]["s"]] = body
def strings_in(x, acc):
    if isinstance(x, str): acc.append(x)
    elif isinstance(x, list): [strings_in(e, acc) for e in x]
    elif isinstance(x, dict) and "cons" in x: [strings_in(e, acc) for e in x["cons"]]
    return acc
def syms_in(x, acc):
    if S(x): acc.append(x["s"])
    elif isinstance(x, list): [syms_in(e, acc) for e in x]
    elif isinstance(x, dict) and "cons" in x: [syms_in(e, acc) for e in x["cons"]]
    return acc
def _calls(x, head, acc=None):
    acc = [] if acc is None else acc
    if isinstance(x, list):
        if x and sym(x[0]) == head: acc.append(x)
        for e in x: _calls(e, head, acc)
    return acc
def eval_str(v):
    if isinstance(v, list) and v and sym(v[0]) == ",": v = v[1]
    if isinstance(v, str): return v
    if S(v):
        rv = resolve_var(v["s"])
        return rv if isinstance(rv, str) else (rv[0] if isinstance(rv, list) and rv else None)
    if isinstance(v, list) and v and sym(v[0]) == "concat":
        parts = [eval_str(p) for p in v[1:]]
        return "".join(parts) if all(isinstance(p, str) for p in parts) else None
    return None
def plist(fm):
    d, i = {}, 1
    while i < len(fm) - 1:
        if S(fm[i]) and fm[i]["s"].startswith(":"): d[fm[i]["s"]] = fm[i+1]; i += 2
        else: i += 1
    return d
def resolve_var(v, depth=0):
    if v not in vars_ or depth > 3: return None
    val = vars_[v]
    if isinstance(val, list) and val and sym(val[0]) == "quote": val = val[1]
    if isinstance(val, str) or (isinstance(val, list) and all(isinstance(e, str) for e in val)): return val
    if S(val): return resolve_var(val["s"], depth+1)
    return None
# language id config
langcfg = []
lic = vars_.get("lsp-language-id-configuration")
if lic:
    body = lic[1] if isinstance(lic, list) and sym(lic[0]) in ("quote", "`") else lic
    for e in body:
        if isinstance(e, dict) and "cons" in e:
            k, v = e["cons"]
            langcfg.append({"match": k["s"] if S(k) else k, "match_kind": "major-mode" if S(k) else "regexp", "language_id": v if isinstance(v, str) else lisp(v)})
json.dump(langcfg, open("out/lsp-mode-language-ids.json", "w"), indent=1)
records = []
for f, forms in loaded:
    for fm in forms:
        if sym(fm[0]) not in ("make-lsp-client", "make-lsp--client"): continue
        p = plist(fm)
        sid = sym(p[":server-id"][1]) if isinstance(p.get(":server-id"), list) else lisp(p.get(":server-id"))
        conn = p.get(":new-connection")
        strs = strings_in(conn, []) if conn is not None else []
        refs = {}
        fun_refs = {}
        for s_ in set(syms_in(conn, [])):
            if s_ in funs:
                body = funs[s_]
                fun_refs[s_] = {"strings": strings_in(body, [])[:12], "executable_find": [a[1] for a in _calls(body, "executable-find") if len(a) > 1 and isinstance(a[1], str)], "vars": {v: resolve_var(v) for v in set(syms_in(body, [])) if resolve_var(v) is not None}}
        for s_ in set(syms_in(conn, [])) | {v for fr in fun_refs.values() for v in fr["vars"]}:
            rv = resolve_var(s_)
            if rv is not None: refs[s_] = rv
        pkgpaths = []
        def find_pkg(x):
            if isinstance(x, list) and x and sym(x[0]) in ("lsp-package-path",) and len(x) > 1:
                n = x[1][1]["s"] if isinstance(x[1], list) and len(x[1]) > 1 and S(x[1][1]) else lisp(x[1])
                pkgpaths.append({"dependency": n, "definitions": deps.get(n)})
            if isinstance(x, list): [find_pkg(e) for e in x]
        find_pkg(conn)
        for fr_name in [s_ for s_ in set(syms_in(conn, [])) if s_ in funs]:
            find_pkg(funs[fr_name])
        sys_bins = []
        for pk in pkgpaths:
            for dfn in pk["definitions"] or []:
                dd = dfn[1] if isinstance(dfn, list) and dfn and sym(dfn[0]) in ("quote", "`") else dfn
                if isinstance(dd, list):
                    for i, e in enumerate(dd[:-1]):
                        if sym(e) in (":system", ":path"):
                            v = dd[i+1]
                            ev = eval_str(v)
                            if ev: sys_bins.append(ev)
            pk["definitions"] = [lisp(d) for d in pk["definitions"] or []]
        act = p.get(":activation-fn")
        activate_on = None
        if isinstance(act, list) and sym(act[0]) == "lsp-activate-on":
            activate_on = [a for a in act[1:] if isinstance(a, str)]
        mm = p.get(":major-modes")
        if isinstance(mm, list) and mm and sym(mm[0]) == "quote": mm = mm[1]
        records.append(rec("lsp-mode", sid,
            file=(os.path.basename(f).split("__")[2] if "EXT__" in f else os.path.relpath(f, "../lsp-mode")),
            package=(os.path.basename(f).split("__")[1] if "EXT__" in f else "lsp-mode (bundled)"),
            connection_raw=lisp(conn) if conn is not None else None,
            strings_in_connection=strs or None,
            resolved_variables=refs or None,
            dependencies=pkgpaths or None,
            dependency_binaries=sys_bins or None,
            called_functions=fun_refs or None,
            major_modes=[m["s"] for m in mm] if isinstance(mm, list) and all(S(m) for m in mm) else (lisp(mm) if mm is not None else None),
            activate_on_languages=activate_on,
            activation_fn_raw=None if activate_on is not None or act is None else lisp(act),
            priority=p.get(":priority", 0) if isinstance(p.get(":priority", 0), int) else lisp(p[":priority"]),
            add_on=p.get(":add-on?") is True,
            remote=p.get(":remote?") is True,
            multi_root=p.get(":multi-root") is True,
            selection={"strategy": "filter by activation/major-mode + binary :test? + enabled/disabled lists; choose max :priority main client, plus all add-on clients"},
            root={"strategy": "shared lsp session folders; lsp-auto-guess-root -> projectile-project-root / project-current; else interactive import prompt"}))
json.dump(records, open("out/lsp-mode.json", "w"), indent=1)
print(len(records), "clients;", len(langcfg), "language-id mappings;", len(deps), "deps")
