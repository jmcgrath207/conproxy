#!/usr/bin/env bash
# Print a summary of the synthetic corpus: topics, product names, sample queries.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="$REPO_ROOT/tests/corpus/data"

cd "$REPO_ROOT"

if [ ! -f "$DATA_DIR/docs.jsonl" ]; then
  echo "  No corpus data found at $DATA_DIR."
  echo "  Run 'cargo run --bin corpus_gen' first, or use 'make dev-restart'."
  exit 0
fi

echo ""
echo "=== Corpus Summary ==="

# Entry counts
echo ""
echo "  Entries:"
for corpus in docs tickets code; do
  count=$(wc -l < "$DATA_DIR/$corpus.jsonl" 2>/dev/null || echo 0)
  printf "    %-8s %3d\n" "$corpus" "$count"
done

# Topics per corpus
echo ""
echo "  Topics:"
for corpus in docs tickets code; do
  f="$DATA_DIR/$corpus.jsonl"
  if [ ! -f "$f" ]; then continue; fi
  topics=$(python3 -c "
import json
topics = set()
with open('$f') as fh:
    for line in fh:
        d = json.loads(line)
        if d.get('topic'):
            topics.add(d['topic'])
print(', '.join(sorted(topics)))
" 2>/dev/null || echo "unable to parse")
  printf "    %-8s %s\n" "$corpus:" "$topics"
done

# Product names (extract from titles)
echo ""
echo "  Product names:"
product_names=$(python3 -c "
import json, re
seen = set()
for corpus in ['docs', 'tickets', 'code']:
    with open('$DATA_DIR/' + corpus + '.jsonl') as fh:
        for line in fh:
            d = json.loads(line)
            t = d.get('title', '')
            # Titles: 'Configuring {product} for {topic}', 'Tuning {product} for {topic}', etc.
            for prefix in ['Configuring ', 'Tuning ', 'Deploying ']:
                if t.startswith(prefix):
                    rest = t[len(prefix):]
                    # Split on ' for', ' on ', ' cache ' etc.
                    name = rest.split(' for')[0].split(' on')[0].split(' cache')[0].split(' mode')[0].split(' implementation')[0].split(' in')[0].strip()
                    if name:
                        seen.add(name)
                    break
print(', '.join(sorted(seen)))
" 2>/dev/null || echo "unable to parse")
echo "    $product_names"

# Sample queries
if [ -f "$DATA_DIR/queries.jsonl" ]; then
  echo ""
  echo "  Sample queries (from queries.jsonl):"
  python3 -c "
import json
with open('$DATA_DIR/queries.jsonl') as fh:
    for i, line in enumerate(fh):
        if i >= 6: break
        d = json.loads(line)
        print(f'    [{d[\"corpus\"]}] \"{d[\"query\"]}\"')
" 2>/dev/null || echo "    unable to parse queries"
fi

echo ""
echo "  Try:  grpcurl -plaintext 127.0.0.1:9999 conproxy.v1.SearchService/Query \\"
echo "          -d '{\"query\":\"cache ttl tuning\",\"top_k\":5}'"
echo ""
