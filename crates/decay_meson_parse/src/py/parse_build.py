import json
from mesonbuild import mparser

SKIP = {"lineno", "colno", "end_lineno", "end_colno", "bytespan",
        "whitespaces", "condition_level", "filename", "pre_whitespaces",
        "commas", "colons"}

def enc(n):
    if isinstance(n, (list, tuple)):
        return [enc(x) for x in n]
    if isinstance(n, dict):
        return [{"key": enc(k), "value": enc(v)} for k, v in n.items()]
    if isinstance(n, (mparser.SymbolNode, mparser.WhitespaceNode)):
        return None                      # rewriter-only, never semantic
    if not isinstance(n, mparser.BaseNode):
        if isinstance(n, (str, int, float, bool)) or n is None:
            return n
        raise TypeError(f"unencodable {type(n).__name__}")
    out = {"kind": type(n).__name__, "line": n.lineno, "col": n.colno}
    for k, v in vars(n).items():
        if k in SKIP or k.startswith("_"):
            continue
        e = enc(v)
        if e is None:                    # prune nulls outright
            continue
        out[k] = e
    return out

def parse(path):
    with open(path, encoding="utf-8") as f:
        src = f.read()
    return json.dumps(enc(mparser.Parser(src, path).parse()))
