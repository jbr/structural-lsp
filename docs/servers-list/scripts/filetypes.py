import json, glob, re, os, xml.etree.ElementTree as ET, tomllib, subprocess
out = {}
# neovim / vim filetype
nv = json.load(open("cache/nvim-filetypes.json"))
m = {}
for ext, ft in nv["extension"].items(): m.setdefault(ft.replace(" (dynamic)", ""), {"extensions": [], "filenames": [], "patterns": []})["extensions"].append(ext)
for fn, ft in nv["filename"].items(): m.setdefault(ft, {"extensions": [], "filenames": [], "patterns": []})["filenames"].append(fn)
for p, ft in nv["pattern"].items(): m.setdefault(ft, {"extensions": [], "filenames": [], "patterns": []})["patterns"].append(p)
out["neovim_filetype"] = {"source": "Neovim 0.12.5 vim.filetype tables (dynamic entries resolved with empty content)", "map": m}
# kate / KSyntaxHighlighting
k = {}
for f in glob.glob("../ksh/data/syntax/*.xml"):
    try:
        root = ET.parse(f).getroot()
    except ET.ParseError:
        txt = open(f, errors="ignore").read(); mm = re.search(r"<language\b[^>]*>", txt)
        if not mm: continue
        root = ET.fromstring(mm.group(0) + "</language>")
    if root.tag != "language": continue
    k[root.get("name")] = {"extensions": [e for e in (root.get("extensions") or "").split(";") if e], "mimetypes": [e for e in (root.get("mimetype") or "").split(";") if e], "section": root.get("section")}
out["kate_highlighting_mode"] = {"source": "KDE/syntax-highlighting data/syntax/*.xml", "map": k}
# kakoune
kk = {}
for f in glob.glob("../kak/rc/filetype/*.kak") + glob.glob("../kak/rc/detection/*.kak"):
    for mm in re.finditer(r"hook global BufCreate\s+(\S+)\s+%\{\s*\n?\s*set-option buffer filetype ([\w-]+)", open(f).read()):
        kk.setdefault(mm.group(2), []).append(mm.group(1))
out["kakoune_filetype"] = {"source": "mawww/kakoune rc/filetype/*.kak BufCreate hooks (buffer-name regexes)", "map": kk}
# candidate extension universe for Emacs regex testing
cands = set(nv["extension"])
for L in tomllib.load(open("../helix-languages.toml", "rb")).get("language", []):
    cands.update(x for x in L.get("file-types", []) if isinstance(x, str))
for v in k.values(): cands.update(e[2:] for e in v["extensions"] if e.startswith("*.") and not re.search(r"[\[\]*?]", e[2:]))
zl = json.load(open("out/zed.json"))["languages"]
for v in zl.values(): cands.update(v["path_suffixes"] or [])
cands = sorted(c for c in cands if c and "/" not in c and "\n" not in c)
open("cache/cand-ext.txt", "w").write("\n".join(cands))
print(len(cands), "candidate extensions;", len(m), "nvim fts;", len(k), "kate modes;", len(kk), "kak fts")
json.dump(out, open("cache/filetypes-partial.json", "w"))
