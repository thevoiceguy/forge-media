#!/bin/bash
# Load testing script for Forge Media Engine
# Tests concurrent sessions, transcoding, and conference operations

set -e

# Configuration
BASE_URL="${FORGE_URL:-http://localhost:8080}"
CONCURRENT_SESSIONS="${SESSIONS:-100}"
DURATION="${DURATION:-60}"
CONFERENCE_ROOMS="${ROOMS:-10}"
PARTICIPANTS_PER_ROOM="${PARTICIPANTS:-5}"

# Colors for output
RED='\033[0:31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Forge Media Engine Load Test${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo "Configuration:"
echo "  Base URL: $BASE_URL"
echo "  Concurrent Sessions: $CONCURRENT_SESSIONS"
echo "  Test Duration: ${DURATION}s"
echo "  Conference Rooms: $CONFERENCE_ROOMS"
echo "  Participants per Room: $PARTICIPANTS_PER_ROOM"
echo ""

# Check if server is running
echo -e "${YELLOW}Checking server health...${NC}"
if ! curl -sf "$BASE_URL/health" > /dev/null; then
    echo -e "${RED}Error: Server not responding at $BASE_URL${NC}"
    echo "Please start the server with: cargo run --release"
    exit 1
fi
echo -e "${GREEN}✓ Server is healthy${NC}"
echo ""

# Function to create a session
create_session() {
    local call_id=$1
    curl -sf -X POST "$BASE_URL/v1/sessions" \
        -H "Content-Type: application/json" \
        -d "{\"call_id\":\"$call_id\"}" > /dev/null
}

# Function to create session with SDP negotiation
create_session_with_sdp() {
    local call_id=$1
    local profile=$2

    # Simple SDP offer for testing
    local sdp_offer="v=0
o=- 1234567890 1234567890 IN IP4 127.0.0.1
s=Test Session
c=IN IP4 127.0.0.1
t=0 0
m=audio 10000 RTP/AVP 0 8
a=rtpmap:0 PCMU/8000
a=rtpmap:8 PCMA/8000"

    curl -sf -X POST "$BASE_URL/v1/sessions" \
        -H "Content-Type: application/json" \
        -d "{
            \"call_id\":\"$call_id\",
            \"sdp_offer\":\"$(echo "$sdp_offer" | sed ':a;N;$!ba;s/\n/\\n/g')\",
            \"local_address\":\"127.0.0.1\",
            \"sdp_profile\":\"$profile\"
        }" > /dev/null
}

# Function to delete a session
delete_session() {
    local call_id=$1
    curl -sf -X DELETE "$BASE_URL/v1/sessions/$call_id" > /dev/null
}

# Function to create conference room
create_room() {
    local room_id=$1
    curl -sf -X POST "$BASE_URL/v1/conferences/$room_id" \
        -H "Content-Type: application/json" \
        -d "{}" > /dev/null
}

# Function to add participant to room
add_participant() {
    local room_id=$1
    local participant_id=$2
    curl -sf -X POST "$BASE_URL/v1/conferences/$room_id/participants" \
        -H "Content-Type: application/json" \
        -d "{\"participant_id\":\"$participant_id\"}" > /dev/null
}

# Function to delete room
delete_room() {
    local room_id=$1
    curl -sf -X DELETE "$BASE_URL/v1/conferences/$room_id" > /dev/null
}

#
# Test 1: Concurrent Session Creation
#
echo -e "${YELLOW}Test 1: Creating $CONCURRENT_SESSIONS concurrent sessions...${NC}"
start_time=$(date +%s)

for i in $(seq 1 $CONCURRENT_SESSIONS); do
    create_session "load-test-$i" &
done
wait

end_time=$(date +%s)
duration=$((end_time - start_time))
echo -e "${GREEN}✓ Created $CONCURRENT_SESSIONS sessions in ${duration}s ($(awk "BEGIN {print $CONCURRENT_SESSIONS/$duration}")sessions/sec)${NC}"
echo ""

#
# Test 2: SDP Negotiation Performance
#
echo -e "${YELLOW}Test 2: Testing SDP negotiation with 50 sessions...${NC}"
start_time=$(date +%s)

for i in $(seq 1 50); do
    create_session_with_sdp "sdp-test-$i" "audio-all" &
done
wait

end_time=$(date +%s)
duration=$((end_time - start_time))
echo -e "${GREEN}✓ Completed 50 SDP negotiations in ${duration}s${NC}"
echo ""

#
# Test 3: Conference Load Test
#
echo -e "${YELLOW}Test 3: Creating $CONFERENCE_ROOMS conference rooms with $PARTICIPANTS_PER_ROOM participants each...${NC}"
start_time=$(date +%s)

for i in $(seq 1 $CONFERENCE_ROOMS); do
    room_id="load-room-$i"
    create_room "$room_id"

    for j in $(seq 1 $PARTICIPANTS_PER_ROOM); do
        add_participant "$room_id" "participant-$i-$j" &
    done
done
wait

end_time=$(date +%s)
duration=$((end_time - start_time))
total_participants=$((CONFERENCE_ROOMS * PARTICIPANTS_PER_ROOM))
echo -e "${GREEN}✓ Created $CONFERENCE_ROOMS rooms with $total_participants total participants in ${duration}s${NC}"
echo ""

#
# Test 4: Fetch Metrics
#
echo -e "${YELLOW}Test 4: Fetching Prometheus metrics...${NC}"
metrics=$(curl -sf "$BASE_URL/metrics")

echo ""
echo -e "${GREEN}Key Metrics:${NC}"
echo "$metrics" | grep -E "forge_active_sessions|forge_conference_rooms_active|forge_conference_participants_active|sdp_negotiation_total|forge_transcoding_packets_total" | head -20
echo ""

#
# Cleanup
#
echo -e "${YELLOW}Cleaning up test resources...${NC}"

# Delete sessions
for i in $(seq 1 $CONCURRENT_SESSIONS); do
    delete_session "load-test-$i" 2>/dev/null &
done

for i in $(seq 1 50); do
    delete_session "sdp-test-$i" 2>/dev/null &
done

# Delete conference rooms
for i in $(seq 1 $CONFERENCE_ROOMS); do
    delete_room "load-room-$i" 2>/dev/null &
done

wait
echo -e "${GREEN}✓ Cleanup complete${NC}"
echo ""

#
# Summary
#
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}Load Test Summary${NC}"
echo -e "${GREEN}========================================${NC}"
echo "Total sessions created: $CONCURRENT_SESSIONS"
echo "SDP negotiations: 50"
echo "Conference rooms: $CONFERENCE_ROOMS"
echo "Total participants: $total_participants"
echo ""
echo -e "${GREEN}✓ All tests completed successfully!${NC}"
echo ""
echo "View full metrics at: $BASE_URL/metrics"
