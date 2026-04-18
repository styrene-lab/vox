#!/usr/bin/env bash
# Review: Overnight Code Reviewer output audit
#
# Run in the morning to inspect the overnight reviewer's work.
# Focuses on the latest session only.

set -uo pipefail

LOG="${HOME}/.local/state/styrene/logs/overnight-reviewer.log"
JOURNAL="${HOME}/workspace/black-meridian/styrene-lab/vox/.omegon/agent-journal.md"
LAUNCHD_OUT="${HOME}/.local/state/styrene/logs/overnight-reviewer.stdout.log"
LAUNCHD_ERR="${HOME}/.local/state/styrene/logs/overnight-reviewer.stderr.log"

echo "═══════════════════════════════════════════════════════════"
echo "  Overnight Reviewer — Morning Audit"
echo "  $(date +%Y-%m-%d)"
echo "═══════════════════════════════════════════════════════════"

# Extract latest session from log (from last "daemon dispatch loop started" to EOF)
LATEST=""
if [ -f "$LOG" ]; then
    LAST_START=$(grep -n "daemon dispatch loop started" "$LOG" | tail -1 | cut -d: -f1)
    if [ -n "$LAST_START" ]; then
        LATEST=$(tail -n +"$LAST_START" "$LOG")
    fi
fi

# ── Journal (session outcome) ────────────────────────────────────
echo ""
echo "── Agent Journal (latest entry) ──"
if [ -f "$JOURNAL" ]; then
    awk '/^## /{buf=""; found=1} found{buf=buf $0 "\n"} END{printf "%s", buf}' "$JOURNAL"
else
    echo "  (no journal found)"
fi

# ── Key metrics from latest session ─────────────────────────────
echo "── Metrics ──"
if [ -n "$LATEST" ]; then
    LOOP=$(echo "$LATEST" | grep "Agent loop complete" | tail -1 | sed 's/.*INFO /  /')
    [ -n "$LOOP" ] && echo "$LOOP" || echo "  (no loop completion)"

    USAGE=$(echo "$LATEST" | grep "total_tokens" | tail -1 | sed 's/.*INFO /  /')
    [ -n "$USAGE" ] && echo "$USAGE"

    PROVIDER=$(echo "$LATEST" | grep "provider telemetry" | tail -1 | sed -n 's/.*provider="\([^"]*\)".*/  provider: \1/p')
    [ -n "$PROVIDER" ] && echo "$PROVIDER"

    PROMPT_LEN=$(echo "$LATEST" | grep "received user prompt" | sed -n 's/.*prompt_len=\([0-9]*\).*/  prompt length: \1 chars/p')
    [ -n "$PROMPT_LEN" ] && echo "$PROMPT_LEN"
else
    echo "  (no session data)"
fi

# ── Errors (latest session only) ─────────────────────────────────
echo ""
echo "── Errors ──"
if [ -n "$LATEST" ]; then
    ERRORS=$(echo "$LATEST" | grep " ERROR " || true)
    if [ -n "$ERRORS" ]; then
        echo "$ERRORS" | sed 's/^/  /'
    else
        echo "  (none)"
    fi
else
    echo "  (no session data)"
fi

# ── Warnings (latest session, deduplicated) ──────────────────────
echo ""
echo "── Warnings (unique, latest session) ──"
if [ -n "$LATEST" ]; then
    WARNS=$(echo "$LATEST" | grep " WARN " | sed 's/^[^ ]* *//' | sort -u || true)
    if [ -n "$WARNS" ]; then
        echo "$WARNS" | head -10 | sed 's/^/  /'
    else
        echo "  (none)"
    fi
else
    echo "  (no session data)"
fi

# ── vox_send / Discord delivery (latest session) ────────────────
echo ""
echo "── Discord Delivery ──"
if [ -n "$LATEST" ]; then
    # Look for extension tool dispatch, not tool_names lists
    VOX=$(echo "$LATEST" | grep -E "execute_send|vox_send.*result|message.*delivered|extension.*dispatch.*send|Stuck detector" || true)
    if [ -n "$VOX" ]; then
        echo "$VOX" | tail -5 | sed 's/^/  /'
    else
        echo "  (no vox_send activity detected)"
    fi
else
    echo "  (no session data)"
fi

# ── Mind facts (latest session) ──────────────────────────────────
echo ""
echo "── Mind Facts ──"
if [ -n "$LATEST" ]; then
    FACTS_BAD=$(echo "$LATEST" | grep "skipping invalid mind fact" || true)
    if [ -n "$FACTS_BAD" ]; then
        echo "  PARSE ERRORS:"
        echo "$FACTS_BAD" | sed 's/^/    /'
    else
        echo "  (all parsed ok)"
    fi
else
    echo "  (no session data)"
fi

# ── Launchd output ───────────────────────────────────────────────
echo ""
echo "── Launchd Output ──"
if [ -f "$LAUNCHD_OUT" ] && [ -s "$LAUNCHD_OUT" ]; then
    echo "  stdout (last 10 lines):"
    tail -10 "$LAUNCHD_OUT" | sed 's/^/    /'
else
    echo "  (no launchd stdout — manual run?)"
fi
if [ -f "$LAUNCHD_ERR" ] && [ -s "$LAUNCHD_ERR" ]; then
    echo "  stderr (last 5 lines):"
    tail -5 "$LAUNCHD_ERR" | sed 's/^/    /'
fi

# ── File locations ───────────────────────────────────────────────
echo ""
echo "── Files ──"
echo "  daemon log:    ${LOG}"
echo "  agent journal: ${JOURNAL}"
echo "  launchd out:   ${LAUNCHD_OUT}"
echo "  launchd err:   ${LAUNCHD_ERR}"
echo ""
echo "  Full log:  less ${LOG}"
echo "  Journal:   cat ${JOURNAL}"
