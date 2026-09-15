import json, subprocess, os
HERE = os.path.dirname(os.path.abspath(__file__))
def dump_forms(path, *heads):
    c = f"{HERE}/cache/{os.path.basename(path)}.jsonl"
    if os.path.exists(c) and os.path.getsize(c) > 0:
        fs = [json.loads(l) for l in open(c) if l.strip()]
        return [f for f in fs if isinstance(f, list) and f and isinstance(f[0], dict) and f[0].get("s") in heads]
    out = subprocess.run([f"{HERE}/emacs.sh", "-l", f"{HERE}/sexp2json.el", path, *heads],
                         capture_output=True, text=True).stdout
    return [json.loads(l) for l in out.splitlines() if l.strip()]
def S(x): return isinstance(x, dict) and "s" in x
def sym(x): return x["s"] if S(x) else None
def lisp(x):
    """render tagged sexp back to readable lisp-ish text"""
    if x is None: return "nil"
    if x is True: return "t"
    if isinstance(x, str): return json.dumps(x)
    if isinstance(x, (int, float)): return str(x)
    if S(x): return x["s"]
    if isinstance(x, dict) and "v" in x: return "[" + " ".join(lisp(e) for e in x["v"]) + "]"
    if isinstance(x, dict) and "cons" in x: return "(" + lisp(x["cons"][0]) + " . " + lisp(x["cons"][1]) + ")"
    if isinstance(x, list):
        if len(x) == 2 and S(x[0]) and x[0]["s"] in ("quote", "function", "`", ","):
            pre = {"quote": "'", "function": "#'", "`": "`", ",": ","}[x[0]["s"]]
            return pre + lisp(x[1])
        return "(" + " ".join(lisp(e) for e in x) + ")"
    return str(x)
def git_rev(repo):
    try:
        return subprocess.run(["git", "-C", repo, "log", "-1", "--format=%H %cs"], capture_output=True, text=True).stdout.strip()
    except Exception: return None
def rec(source, server_id, **kw):
    r = {"source": source, "server_id": server_id}
    r.update(kw); return r
