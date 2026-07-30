#!/usr/bin/env python3
"""Convert eval_results.json → eval_results.html (standalone, no dependencies).

Usage:
    render_html.py <eval_results.json> [output.html]

Automatically loads eval_docs.json from tests/e2e/data/ to show expected content.
Also shows agent response_text if present in the JSON.
"""

import json
import sys
import os

def html_escape(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")

def conf_class(c: float) -> str:
    if c >= 0.5: return "conf-high"
    if c >= 0.3: return "conf-mid"
    return "conf-low"

def tag_class(t: str) -> str:
    if t.startswith("phrase:"): return "tag tag-phrase"
    if t.startswith("title"):   return "tag tag-title"
    if t.startswith("tag:"):    return "tag tag-tag"
    return "tag"

# Short name map
SHORT = {
    "no_context": "No Ctx",
    "knowledge_only": "Know Only",
    "cli_search": "CLI Search",
    "mcp_tools": "MCP+Tools",
}

CSS = """\
  :root { --bg: #0d1117; --surface: #161b22; --border: #30363d; --fg: #e6edf3;
          --fg2: #8b949e; --green: #3fb950; --red: #f85149; --yellow: #d29922;
          --blue: #58a6ff; --purple: #bc8cff; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, monospace;
         background: var(--bg); color: var(--fg); padding: 2rem; line-height: 1.5; }
  h1 { font-size: 1.6rem; margin-bottom: .3rem; }
  .subtitle { color: var(--fg2); font-size: .85rem; margin-bottom: 1.5rem; }
  h2 { font-size: 1.15rem; margin: 1.8rem 0 .8rem; color: var(--blue); }
  table { width: 100%; border-collapse: collapse; margin-bottom: 1.2rem; }
  th, td { padding: .55rem .75rem; text-align: left; border: 1px solid var(--border); }
  th { background: var(--surface); font-weight: 600; font-size: .85rem; color: var(--fg2); }
  td { font-size: .85rem; }
  tr:hover td { background: #1c2333; }
  .best { background: #0d2818; }
  .worst { background: #2d1114; }
  .pass { color: var(--green); font-weight: 600; }
  .fail { color: var(--red); font-weight: 600; }
  .timeout { color: var(--yellow); font-weight: 600; }
  .conf-high { color: var(--green); }
  .conf-mid { color: var(--yellow); }
  .conf-low { color: var(--red); }
  .badge { display: inline-block; padding: .15rem .5rem; border-radius: 3px;
           font-size: .75rem; font-weight: 600; }
  .badge-best { background: #0d2818; color: var(--green); border: 1px solid var(--green); }
  .badge-worst { background: #2d1114; color: var(--red); border: 1px solid var(--red); }
  .bar-cell { position: relative; }
  .bar { position: absolute; top: 0; left: 0; bottom: 0; opacity: .15; border-radius: 2px; }
  .bar-green { background: var(--green); }
  details { margin: .4rem 0; }
  summary { cursor: pointer; font-size: .8rem; color: var(--fg2); padding: .2rem 0; }
  summary:hover { color: var(--fg); }
  .tags { display: flex; flex-wrap: wrap; gap: .3rem; margin-top: .3rem; }
  .tag { font-size: .7rem; padding: .1rem .4rem; border-radius: 3px;
         background: var(--surface); border: 1px solid var(--border); color: var(--fg2); }
  .tag-phrase { border-color: var(--purple); color: var(--purple); }
  .tag-title { border-color: var(--blue); color: var(--blue); }
  .tag-tag { border-color: var(--green); color: var(--green); }
  .query-text { color: var(--fg2); font-size: .8rem; font-style: italic; display: block;
                margin-top: .2rem; max-width: 60ch; }
  .card { background: var(--surface); border: 1px solid var(--border); border-radius: 6px;
          padding: 1rem 1.2rem; margin-bottom: 1rem; }
  .grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
  @media (max-width: 800px) { .grid-2 { grid-template-columns: 1fr; } }
  .metric-val { font-size: 1.8rem; font-weight: 700; }
  .metric-label { font-size: .75rem; color: var(--fg2); text-transform: uppercase; letter-spacing: .05em; }
  .response-section { margin-top: .6rem; }
  .response-label { font-size: .75rem; font-weight: 600; color: var(--fg2); text-transform: uppercase;
                     letter-spacing: .05em; margin-bottom: .3rem; }
  .response-box { background: var(--surface); border: 1px solid var(--border); border-radius: 4px;
                  padding: .6rem .8rem; font-size: .8rem; white-space: pre-wrap; word-break: break-word;
                  max-height: 20rem; overflow-y: auto; line-height: 1.5; }
  .response-box.expected { border-left: 3px solid var(--green); }
  .response-box.agent { border-left: 3px solid var(--blue); }
  .no-response { color: var(--fg2); font-style: italic; font-size: .8rem; }
"""


def render(data: dict, docs: dict[str, dict]) -> str:
    verticals = data["verticals"]
    comparison = data.get("comparison", {})
    best_name = comparison.get("best_vertical", "")
    worst_name = comparison.get("worst_vertical", "")
    timestamp = data.get("timestamp", "")

    lines = []
    w = lines.append

    w(f'<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
      f'<meta name="viewport" content="width=device-width, initial-scale=1">\n'
      f'<title>Conpack E2E Eval Results</title>\n<style>\n{CSS}</style>\n</head>\n<body>')

    w(f'<h1>Conpack E2E Eval Results</h1>')
    w(f'<p class="subtitle">Source: {html_escape(timestamp)}</p>')

    # --- Summary cards ---
    w('<h2>Summary</h2>')
    w('<div class="grid-2">')
    for v in verticals:
        name = v["name"]
        sn = SHORT.get(name, name)
        recall = float(v["total_recall"]) * 100
        avg_s = v["avg_response_time_ms"] / 1000
        ok = v["successful_queries"]
        total = ok + v["failed_queries"]
        tmo = v["timed_out_queries"]

        rc = conf_class(float(v["total_recall"]))
        badge = ""
        if name == best_name:
            badge = ' <span class="badge badge-best">BEST</span>'
        elif name == worst_name:
            badge = ' <span class="badge badge-worst">WORST</span>'

        w(f'<div class="card">'
          f'<div class="metric-label">{sn}{badge}</div>'
          f'<div class="metric-val {rc}">{recall:.0f}%</div>'
          f'<div class="metric-label">recall &nbsp;|&nbsp; {avg_s:.1f}s avg &nbsp;|&nbsp; {ok}/{total} ok &nbsp;|&nbsp; {tmo} timeouts</div>'
          f'</div>')
    w('</div>')

    # --- Comparison table ---
    w('<h2>Comparison</h2>')
    w('<table><tr><th>Metric</th>')
    for v in verticals:
        w(f'<th>{SHORT.get(v["name"], v["name"])}</th>')
    w('</tr>')

    w('<tr><td>Total Recall</td>')
    for v in verticals:
        pct = float(v["total_recall"]) * 100
        cls = ""
        if v["name"] == best_name: cls = ' class="best"'
        elif v["name"] == worst_name: cls = ' class="worst"'
        w(f'<td{cls} class="bar-cell"><span class="bar bar-green" style="width:{pct:.0f}%"></span><span class="bar-label">{pct:.1f}%</span></td>')
    w('</tr>')

    w('<tr><td>Avg Response Time</td>')
    for v in verticals:
        w(f'<td>{v["avg_response_time_ms"]/1000:.1f}s</td>')
    w('</tr>')

    w('<tr><td>Success Rate</td>')
    for v in verticals:
        total = v["successful_queries"] + v["failed_queries"]
        w(f'<td>{v["successful_queries"]}/{total}</td>')
    w('</tr>')

    w('<tr><td>Timeouts</td>')
    for v in verticals:
        cls = ' class="timeout"' if v["timed_out_queries"] > 0 else ''
        w(f'<td{cls}>{v["timed_out_queries"]}</td>')
    w('</tr></table>')

    # --- Per-query recall ---
    qids = []
    for v in verticals:
        for q in v["queries"]:
            if q["query_id"] not in qids:
                qids.append(q["query_id"])

    w('<h2>Per-Query Recall</h2>')
    w('<table><tr><th>Query</th>')
    for v in verticals:
        w(f'<th>{SHORT.get(v["name"], v["name"])}</th>')
    w('</tr>')

    for qid in qids:
        qt = _query_text(verticals, qid)
        w(f'<tr><td><strong>{qid}</strong><span class="query-text">{html_escape(qt)}</span></td>')
        for v in verticals:
            q = _find_query(v, qid)
            if q is None:
                w('<td>-</td>')
                continue
            if q.get("timed_out"):
                w('<td class="timeout">TIMEOUT</td>')
            elif not q["success"]:
                w('<td class="fail">ERR</td>')
            elif float(q["recall"]) >= 0.99:
                w(f'<td class="pass">{float(q["recall"])*100:.0f}%</td>')
            else:
                w(f'<td class="fail">{float(q["recall"])*100:.0f}%</td>')
        w('</tr>')
    w('</table>')

    # --- Query Details (scoring + expected + responses) ---
    w('<h2>Query Details</h2>')
    for qid in qids:
        qt = _query_text(verticals, qid)
        w(f'<details><summary><strong>{qid}</strong> — {html_escape(qt)}</summary>')

        # Expected document content
        first_q = None
        for v in verticals:
            first_q = _find_query(v, qid)
            if first_q: break
        if first_q:
            for d in first_q.get("doc_scores", []):
                doc_id = d["doc_id"]
                doc = docs.get(doc_id)
                if doc:
                    w(f'<div class="response-section">'
                      f'<div class="response-label">Expected: {html_escape(doc_id)} — {html_escape(doc["title"])}</div>'
                      f'<div class="response-box expected">{html_escape(doc["content"])}</div></div>')

        # Scoring table
        w('<table><tr><th>Vertical</th><th>Doc</th><th>Match</th><th>Confidence</th><th>Time</th><th>Matched Terms</th></tr>')
        for v in verticals:
            sn = SHORT.get(v["name"], v["name"])
            q = _find_query(v, qid)
            if q is None:
                continue
            time_s = q["response_time_ms"] / 1000
            for d in q.get("doc_scores", []):
                mc = "pass" if d["matched"] else "fail"
                ml = "YES" if d["matched"] else "NO"
                conf = float(d["confidence"])
                cc = conf_class(conf)
                tags_html = "".join(
                    f'<span class="{tag_class(t)}">{html_escape(t)}</span>'
                    for t in d.get("matched_terms", [])
                )
                w(f'<tr><td>{sn}</td><td>{d["doc_id"]}</td><td class="{mc}">{ml}</td>'
                  f'<td class="{cc}">{conf:.3f}</td><td>{time_s:.1f}s</td>'
                  f'<td><div class="tags">{tags_html}</div></td></tr>')
        w('</table>')

        # Agent responses per vertical
        w('<div class="response-section"><div class="response-label">Agent Responses</div></div>')
        for v in verticals:
            sn = SHORT.get(v["name"], v["name"])
            q = _find_query(v, qid)
            if q is None:
                continue
            if q.get("timed_out"):
                status = "TIMEOUT"
            elif not q["success"]:
                status = "FAILED"
            else:
                status = f'recall {float(q["recall"])*100:.0f}%'

            resp = q.get("response_text", "")
            w(f'<details><summary>{sn} — {status}</summary>')
            if resp:
                w(f'<div class="response-box agent">{html_escape(resp)}</div>')
            else:
                w('<p class="no-response">No response captured (older result format)</p>')
            w('</details>')

        w('</details>')

    w('</body></html>')
    return "\n".join(lines)


def _find_query(vertical: dict, qid: str):
    return next((q for q in vertical["queries"] if q["query_id"] == qid), None)


def _query_text(verticals: list, qid: str) -> str:
    for v in verticals:
        for q in v["queries"]:
            if q["query_id"] == qid:
                return q["query_text"]
    return ""


def load_eval_docs(json_path: str) -> dict[str, dict]:
    """Try to find eval_docs.json relative to the results file or the project root."""
    candidates = []

    # Walk up from json_path looking for tests/e2e/data/eval_docs.json
    d = os.path.dirname(os.path.abspath(json_path))
    for _ in range(10):
        candidate = os.path.join(d, "tests", "e2e", "data", "eval_docs.json")
        candidates.append(candidate)
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent

    # Also try relative to this script
    script_dir = os.path.dirname(os.path.abspath(__file__))
    candidates.append(os.path.join(script_dir, "..", "e2e", "data", "eval_docs.json"))

    for path in candidates:
        if os.path.exists(path):
            with open(path) as f:
                data = json.load(f)
            return {doc["id"]: doc for doc in data["documents"]}

    print("Warning: eval_docs.json not found, expected doc content will not be shown", file=sys.stderr)
    return {}


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <eval_results.json> [output.html]", file=sys.stderr)
        sys.exit(1)

    json_path = sys.argv[1]
    html_path = sys.argv[2] if len(sys.argv) > 2 else json_path.replace(".json", ".html")

    with open(json_path) as f:
        data = json.load(f)

    docs = load_eval_docs(json_path)
    html = render(data, docs)

    with open(html_path, "w") as f:
        f.write(html)

    print(f"Written: {html_path}", file=sys.stderr)

if __name__ == "__main__":
    main()
