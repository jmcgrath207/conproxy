#!/usr/bin/env bash
# Post-process hook for `conproxy distill`.
# Usage:
#   conproxy distill --output-dir /tmp/distill --post-process "./examples/distill-postprocess.sh"
#
# Env vars set by conproxy: DISTILL_OUTPUT_DIR, DISTILL_FILE_COUNT,
# DISTILL_INDEX_MD, DISTILL_INDEX_JSON

echo "=== distill summary ==="
echo "output dir: ${DISTILL_OUTPUT_DIR}"
echo "files written: ${DISTILL_FILE_COUNT}"
echo "index md: ${DISTILL_INDEX_MD}"
echo "index json: ${DISTILL_INDEX_JSON}"

if [ -n "${DISTILL_OUTPUT_DIR}" ] && [ -d "${DISTILL_OUTPUT_DIR}" ]; then
    echo ""
    echo "=== entry files ==="
    ls -1 "${DISTILL_OUTPUT_DIR}"/*.md 2>/dev/null | grep -v _index.md | head -20
fi
