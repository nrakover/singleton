#!/usr/bin/env bash
set -euo pipefail

THREAD_DIR="$SINGLETON_STATE_DIR/threads/$SINGLETON_THREAD_ID"
TIMESTAMP_MS=$(python3 -c "import time; print(int(time.time()*1000))")
RANDOM4=$(python3 -c "import secrets; print(secrets.token_hex(2))")
EVENT_ID="${TIMESTAMP_MS}-notify-${RANDOM4}"

# Read stdin
STDIN_DATA=$(cat)
MESSAGE=$(echo "$STDIN_DATA" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('message',''))" 2>/dev/null || echo "")

python3 - <<EOF
import json, datetime
data = {
    "event_id": "$EVENT_ID",
    "thread_id": "$SINGLETON_THREAD_ID",
    "type": "notification",
    "data": {"message": "$MESSAGE"},
    "timestamp": datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%S.000Z")
}
with open("$THREAD_DIR/events/$EVENT_ID.json", "w") as f:
    json.dump(data, f)
EOF

exit 0
