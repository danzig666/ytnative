#!/bin/sh
# Zero-code playback test: hand a YouTube HLS manifest to the TV's media service (uMediaServer).
# Runs from /tmp/ytnative only; writes nothing else. Always unloads the pipeline at the end.
cd /tmp/ytnative || exit 1
URL=$(sed -n 3p last_streams.txt)
SECS=${1:-30}
LOAD=$(printf '{"uri":"%s","type":"media","payload":{"option":{"appId":"com.webos.app.home","mediaTransportType":"HLS"}}}' "$URL")
R=$(luna-send -w 8000 -n 1 luna://com.webos.media/load "$LOAD")
echo "load -> $R"
MID=$(echo "$R" | sed -n 's/.*"mediaId": *"\([^"]*\)".*/\1/p')
[ -z "$MID" ] && { echo "no mediaId"; exit 1; }
# Log pipeline events in the background while it plays.
luna-send -w $((SECS*1000+4000)) -i luna://com.webos.media/subscribe "{\"mediaId\":\"$MID\"}" > events.log 2>&1 &
sleep 1
echo "play -> $(luna-send -w 5000 -n 1 luna://com.webos.media/play "{\"mediaId\":\"$MID\"}")"
sleep "$SECS"
echo "unload -> $(luna-send -w 5000 -n 1 luna://com.webos.media/unload "{\"mediaId\":\"$MID\"}")"
sleep 2
echo "--- events (first 40 lines) ---"
head -c 6000 events.log | head -40
