const ts = require("typescript"), fs = require("fs");
const src = fs.readFileSync("../oc-server.ts", "utf8");
const sf = ts.createSourceFile("server.ts", src, ts.ScriptTarget.Latest, true);
const text = n => n.getText(sf);
const strArr = n => n && ts.isArrayLiteralExpression(n) ? n.elements.filter(ts.isStringLiteralLike).map(e => e.text) : null;
function collectCalls(node, acc) {
  if (ts.isCallExpression(node)) {
    const callee = text(node.expression);
    const args = node.arguments.map(a => ts.isStringLiteralLike(a) ? a.text : null);
    if (/(^|\.)which$|resolve$|install|spawn$|^\$|fetch/i.test(callee)) acc.push({ callee, args });
  }
  ts.forEachChild(node, c => { collectCalls(c, acc); });
}
function strings(node, acc) { if (ts.isStringLiteralLike(node) || ts.isNoSubstitutionTemplateLiteral(node)) acc.add(node.text); ts.forEachChild(node, c => { strings(c, acc); }); return acc; }
const out = [];
sf.statements.forEach(st => {
  if (!ts.isVariableStatement(st)) return;
  st.declarationList.declarations.forEach(d => {
    if (!d.type || text(d.type) !== "Info" || !d.initializer || !ts.isObjectLiteralExpression(d.initializer)) return;
    const r = { source: "opencode", export_name: text(d.name) };
    d.initializer.properties.forEach(p => {
      const name = p.name && text(p.name);
      if (name === "id") r.server_id = p.initializer.text;
      else if (name === "extensions") r.extensions = strArr(p.initializer);
      else if (name === "global") r.global = text(p.initializer);
      else if (name === "root") {
        const init = p.initializer;
        if (ts.isCallExpression(init) && /NearestRoot$/.test(text(init.expression))) {
          r.root = { helper: text(init.expression), include: strArr(init.arguments[0]), exclude: strArr(init.arguments[1]) };
        } else if (init) { r.root = { helper: "custom", source: text(init), string_literals: [...strings(init, new Set())] }; }
      } else if (name === "spawn" || (ts.isMethodDeclaration(p) && name === "spawn")) {
        const calls = []; collectCalls(p, calls);
        r.spawn_calls = calls;
        r.spawn_string_literals = [...strings(p, new Set())].filter(s => s.length < 80).slice(0, 40);
        r.spawn_source_lines = text(p).split("\n").length;
      }
    });
    if (!r.root && /root/.test(text(d.initializer))) r.root = { helper: "unparsed" };
    out.push(r);
  });
});
// top-level helper functions referenced (e.g. roslyn install helpers)
fs.writeFileSync("out/opencode.raw.json", JSON.stringify(out, null, 1));
console.log(out.length, out.filter(r => !r.root).map(r => r.server_id));
