from common import *
import glob, re
forms = dump_forms("../lsp-bridge/lsp-bridge.el", "defcustom", "defvar")
V = {}; DOC = {}
for f in forms:
    if len(f) > 2 and S(f[1]):
        V[f[1]["s"]] = f[2]
        if len(f) > 3 and isinstance(f[3], str): DOC[f[1]["s"]] = f[3]
def unq(x): return x[1] if isinstance(x, list) and x and sym(x[0]) in ("quote", "`") else x
def resolve(val):
    if isinstance(val, str): return val, None
    if S(val):
        d = unq(V.get(val["s"]))
        return (d if isinstance(d, str) else lisp(d)), val["s"]
    return lisp(val), None
def table(name, keykind):
    rows = []
    for e in unq(V[name]) or []:
        if isinstance(e, dict) and "cons" in e: k, v = e["cons"]
        elif isinstance(e, list) and len(e) == 2 and not (S(e[0]) and len(e)==2 and False): k, v = e[0], e[1]
        else:  # e.g. bare hook symbol leftovers
            continue
        keys = [k["s"]] if S(k) else [x["s"] if S(x) else x for x in k] if isinstance(k, list) else [lisp(k)]
        server, var = resolve(v)
        rows.append({"match": keys, "match_kind": keykind, "server": server, "via_variable": var,
                     "variable_doc": DOC.get(var) if var else None})
    return rows
mappings = {
  "single_by_mode": table("lsp-bridge-single-lang-server-mode-list", "major-mode"),
  "multi_by_mode": table("lsp-bridge-multi-lang-server-mode-list", "major-mode"),
  "single_by_extension": table("lsp-bridge-single-lang-server-extension-list", "extension"),
  "multi_by_extension": table("lsp-bridge-multi-lang-server-extension-list", "extension"),
}
# choice variables: every defcustom ending in -lsp-server
choices = {k: {"default": unq(V[k]) if isinstance(unq(V[k]), str) else lisp(V[k]), "doc": DOC.get(k)}
           for k in V if re.search(r"-lsp-server$", k)}
servers = {}
for f in sorted(glob.glob("../lsp-bridge/langserver/*.json")):
    d = json.load(open(f)); base = os.path.basename(f)[:-5]
    servers[base] = d
multis = {os.path.basename(f)[:-5]: json.load(open(f)) for f in sorted(glob.glob("../lsp-bridge/multiserver/*.json"))}
records = []
ROOT = "lsp-bridge-get-project-path-by-filepath user fn -> nearest .dir-locals.el -> git rev-parse --show-toplevel -> file itself (single-file mode); servers with support-single-file=false then search projectFiles up to 20 levels or refuse"
for name, d in servers.items():
    platform = None
    m = re.match(r"(.*)_(windows|darwin)$", name)
    if m: platform = m.group(2)
    lookup = m.group(1) if m else name
    refs = []
    for kind, rows in mappings.items():
        for r in rows:
            if r["server"] == lookup: refs.append({"table": kind, **{k: r[k] for k in ("match","match_kind","via_variable")}})
    for mname, md in multis.items():
        members = [v for val in md.values() for v in ([val] if isinstance(val, str) else val)]
        if lookup in members:
            refs.append({"table": "member_of_multiserver", "multiserver": mname})
    alt_of = [k for k, c in choices.items() if c["doc"] and re.search(r"(?<![\w-])" + re.escape(lookup) + r"(?![\w-])", c["doc"])]
    records.append(rec("lsp-bridge", d.get("name", lookup), file=f"langserver/{name}.json", platform=platform,
        command=d.get("command"), executables=[d["command"][0]] if d.get("command") else [],
        language_id=d.get("languageId"), language_ids=d.get("languageIds"),
        project_files=d.get("projectFiles"), support_single_file=d.get("support-single-file"),
        referenced_by=refs or None, listed_as_choice_in=alt_of or None,
        selection={"strategy": "user hook -> extension table -> major-mode table; multi-server tried first and only used if ALL member commands pass shutil.which, else single server"},
        root={"strategy": ROOT, "markers": d.get("projectFiles")}))
out = {"records": records, "mappings": mappings, "choice_variables": choices, "multiservers": multis}
json.dump(out, open("out/lsp-bridge.json", "w"), indent=1)
print(len(records), "servers", {k: len(v) for k, v in mappings.items()}, len(choices), "choice vars", len(multis), "multi")
print("unreferenced:", [r["server_id"] for r in records if not r["referenced_by"] and not r["listed_as_choice_in"]])
