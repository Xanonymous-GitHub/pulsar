#!/bin/sh
# Checks a running pulsar board against its MQTT broker, from a computer: is it online, is its
# state message complete, does Home Assistant discovery parse, do the commands answer, are long
# messages put back together and too long ones dropped. Needs mosquitto_sub, mosquitto_pub
# (`brew install mosquitto`) and python3. See "Check a board" in docs/mqtt.md.
#
#   tools/mqtt-check.sh -h broker.example.org [-p 1883] [-u user -P password-file] [-i a1b2c3]
#                       [--tls] [--restart]
#
#   -i       the board's id (the last three bytes of its MAC address). Found by itself if only
#            one board is on the broker.
#   --tls    connect with TLS, checking the broker's certificate against the system's roots.
#   --restart  also send the restart command and wait for the board to come back (takes a minute).
#
# The password is read from a file so that it is not in the process list or the shell history.
set -u

host= port= user= password_file= id= tls=0 restart=0
while [ $# -gt 0 ]; do
    case "$1" in
        -h) host=$2; shift 2 ;;
        -p) port=$2; shift 2 ;;
        -u) user=$2; shift 2 ;;
        -P) password_file=$2; shift 2 ;;
        -i) id=$2; shift 2 ;;
        --tls) tls=1; shift ;;
        --restart) restart=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
[ -n "$host" ] || { sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
[ -n "$port" ] || { [ "$tls" = 1 ] && port=8883 || port=1883; }

work=$(mktemp -d) || exit 2
trap 'rm -rf "$work"' EXIT INT TERM

# The options every mosquitto client call gets, kept in a config-free way: the password goes
# through the environment of this script only.
set -- -h "$host" -p "$port" -V 5
[ "$tls" = 1 ] && set -- "$@" --tls-use-os-certs
if [ -n "$user" ]; then
    set -- "$@" -u "$user"
    [ -n "$password_file" ] && set -- "$@" -P "$(cat "$password_file")"
fi

passed=0 failed=0
pass() { passed=$((passed + 1)); printf 'PASS  %s\n' "$1"; }
fail() { failed=$((failed + 1)); printf 'FAIL  %s\n' "$1"; [ $# -gt 1 ] && printf '      %s\n' "$2"; }

# The payload of the first message on a topic within some seconds (a retained one comes at once).
first() { mosquitto_sub "$@" -C 1 -W "$wait" -t "$topic" 2>/dev/null; }

# --- which board
if [ -z "$id" ]; then
    id=$(mosquitto_sub "$@" -C 1 -W 70 -t 'pulsar/+/availability' -F '%t' 2>/dev/null | cut -d/ -f2)
    [ -n "$id" ] || { fail "a board is on the broker" "nothing on pulsar/+/availability within 70 s"; exit 1; }
fi
base="pulsar/$id"
echo "board: $id on $host:$port"

# --- availability
topic="$base/availability" wait=5
value=$(first "$@")
[ "$value" = online ] && pass "availability is 'online' (retained)" || fail "availability is 'online' (retained)" "got '$value'"

# --- state
topic="$base/state" wait=5
first "$@" > "$work/state"
if python3 - "$work/state" <<'PY'
import json, sys
state = json.load(open(sys.argv[1]))
need = {"ssid": str, "rssi": int, "band": str, "ip": str, "uptime_s": int, "free_heap": int, "min_free_heap": int}
bad = [key for key, kind in need.items() if not isinstance(state.get(key), kind)]
sys.exit(f"missing or wrong type: {bad}" if bad else 0)
PY
then pass "state is retained JSON with every field"; else fail "state is retained JSON with every field" "$(head -c 200 "$work/state")"; fi

# --- Home Assistant discovery
topic="homeassistant/device/pulsar-$id/config" wait=5
first "$@" > "$work/discovery"
if python3 - "$work/discovery" "$base" <<'PY'
import json, sys
config = json.load(open(sys.argv[1]))
text = json.dumps(config)
sys.exit(0 if sys.argv[2] in text and len(config) >= 3 else "does not mention the board's topics")
PY
then pass "Home Assistant discovery is retained JSON ($(wc -c < "$work/discovery" | tr -d ' ') bytes)"; else fail "Home Assistant discovery is retained JSON"; fi

# --- ping: what goes to cmd/ping must come back on pong, byte for byte
echo_test() { # echo_test <description> <payload file> <qos> <echo|silence> -- client options
    description=$1 payload=$2 qos=$3 expect=$4; shift 5
    mosquitto_sub "$@" -C 1 -W 12 -t "$base/pong" > "$work/pong" 2>/dev/null &
    listener=$!
    # Give the subscription time to reach the broker before the command goes out.
    python3 -c 'import time; time.sleep(1.5)'
    mosquitto_pub "$@" -q "$qos" -t "$base/cmd/ping" -f "$payload"
    wait "$listener"
    if [ "$expect" = echo ]; then
        # mosquitto_sub adds a line break after the payload: compare the payload's length only.
        head -c "$(wc -c < "$payload")" "$work/pong" > "$work/pong.raw"
        cmp -s "$payload" "$work/pong.raw" && pass "$description" || fail "$description" "sent $(wc -c < "$payload" | tr -d ' ') bytes, got $(wc -c < "$work/pong" | tr -d ' ') back"
    else
        [ -s "$work/pong" ] && fail "$description" "got $(wc -c < "$work/pong" | tr -d ' ') bytes back" || pass "$description"
    fi
    rm -f "$work/pong" "$work/pong.raw"
}
text() { python3 -c 'import sys; n=int(sys.argv[1]); sys.stdout.write(("pulsar-check-" + sys.argv[2] + "-") + "x" * (n - 14 - len(sys.argv[2])))' "$1" "$2" > "$work/payload-$2"; echo "$work/payload-$2"; }

echo_test "ping answers on pong (QoS 0)" "$(text 24 q0)" 0 echo -- "$@"
echo_test "ping answers on pong (QoS 1)" "$(text 24 q1)" 1 echo -- "$@"
echo_test "ping answers on pong (QoS 2)" "$(text 24 q2)" 2 echo -- "$@"
echo_test "a 3000-byte message is put back together" "$(text 3000 long)" 1 echo -- "$@"
echo_test "a 6000-byte message is put back together" "$(text 6000 longest)" 1 echo -- "$@"
echo_test "a 7000-byte message is dropped (over the 6 KiB limit)" "$(text 7000 toolong)" 1 silence -- "$@"
echo_test "the board still answers after the dropped message" "$(text 24 after)" 1 echo -- "$@"

# --- the notice command is accepted (what the screen shows cannot be seen from here)
mosquitto_pub "$@" -q 1 -t "$base/cmd/notice" -m "mqtt-check was here" && pass "notice command sent (look at the OLED: 'mqtt-check was here' for 30 s)" || fail "notice command sent"

# --- a new state message arrives by itself, with a later uptime
topic="$base/state" wait=75
before=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["uptime_s"])' "$work/state" 2>/dev/null || echo 0)
mosquitto_sub "$@" -R -C 1 -W 75 -t "$topic" > "$work/state2" 2>/dev/null
after=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["uptime_s"])' "$work/state2" 2>/dev/null || echo 0)
[ "$after" -gt "$before" ] 2>/dev/null && pass "a fresh state message arrives within 75 s (uptime $before s -> $after s)" || fail "a fresh state message arrives within 75 s" "uptime before $before, after '$after'"

# --- restart: offline on purpose, back online, and the command does not stay on the broker
if [ "$restart" = 1 ]; then
    mosquitto_sub "$@" -R -W 90 -t "$base/availability" > "$work/availability" 2>/dev/null &
    listener=$!
    python3 -c 'import time; time.sleep(1.5)'
    mosquitto_pub "$@" -q 1 -r -t "$base/cmd/restart" -m now
    wait "$listener"
    sequence=$(tr '\n' ' ' < "$work/availability")
    case "$sequence" in
        *offline*online*) pass "restart: availability went offline, then online" ;;
        *) fail "restart: availability went offline, then online" "saw: $sequence" ;;
    esac
    topic="$base/cmd/restart" wait=3
    leftover=$(first "$@")
    [ -z "$leftover" ] && pass "restart: the retained command was cleared from the broker" || fail "restart: the retained command was cleared from the broker" "still there: '$leftover'"
fi

echo
echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
