#!/usr/bin/env bash
set -euo pipefail

THREAD_DIR="$SINGLETON_STATE_DIR/threads/$SINGLETON_THREAD_ID"
TIMESTAMP_MS=$(python3 -c "import time; print(int(time.time()*1000))")
RANDOM4=$(python3 -c "import secrets; print(secrets.token_hex(2))")
REQ_ID="req_${TIMESTAMP_MS}_${RANDOM4}"
EVENT_ID="${TIMESTAMP_MS}-pretool-${RANDOM4}"

# Timeout (default 300 seconds, can be overridden for testing)
TIMEOUT_SECS="${SINGLETON_PRETOOL_TIMEOUT:-300}"

# Read stdin
STDIN_DATA=$(cat)

# Check permissions mode
MODE=$(python3 -c "import json; d=json.load(open('$THREAD_DIR/thread.json')); print(d['permissions_mode'])")

if [ "$MODE" = "yolo" ]; then
    exit 0
fi

# Extract tool info from stdin
TOOL_NAME=$(echo "$STDIN_DATA" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('tool_name',''))" 2>/dev/null || echo "unknown")
TOOL_INPUT=$(echo "$STDIN_DATA" | python3 -c "import sys,json; d=json.load(sys.stdin); print(json.dumps(d.get('tool_input',{})))" 2>/dev/null || echo "{}")

# Write pending request
python3 - <<EOF
import json, datetime
data = {
    "request_id": "$REQ_ID",
    "thread_id": "$SINGLETON_THREAD_ID",
    "tool": "$TOOL_NAME",
    "input": $TOOL_INPUT,
    "mode": "$MODE",
    "created_at": datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%S.000Z")
}
with open("$THREAD_DIR/pending/$REQ_ID.json", "w") as f:
    json.dump(data, f)
EOF

# Write pretool event
python3 - <<EOF
import json, datetime
data = {
    "event_id": "$EVENT_ID",
    "thread_id": "$SINGLETON_THREAD_ID",
    "type": "pretool",
    "data": {
        "request_id": "$REQ_ID",
        "tool": "$TOOL_NAME",
        "input": $TOOL_INPUT,
        "mode": "$MODE"
    },
    "timestamp": datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%S.000Z")
}
with open("$THREAD_DIR/events/$EVENT_ID.json", "w") as f:
    json.dump(data, f)
EOF

# Poll for response
for i in $(seq 1 "$TIMEOUT_SECS"); do
    RESPONSE_FILE="$THREAD_DIR/responses/${REQ_ID}.json"
    if [ -f "$RESPONSE_FILE" ]; then
        DECISION=$(python3 -c "import json; d=json.load(open('$RESPONSE_FILE')); print(d['decision'])")
        if [ "$DECISION" = "approve" ]; then
            exit 0
        else
            echo "Tool call denied by $MODE approval"
            exit 2
        fi
    fi
    sleep 1
done

echo "Approval timeout after $TIMEOUT_SECS seconds"
exit 2
