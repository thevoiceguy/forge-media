#!/bin/bash
# Test script for TOS/QoS configuration feature

set -e

echo "========================================="
echo "TOS/QoS Configuration Test Suite"
echo "========================================="
echo ""

BASE_URL="http://localhost:8080"

echo "1. Testing Default TOS (should use global config: 0xB8/184)"
echo "   Creating session without TOS parameter..."
RESPONSE1=$(curl -s -X POST $BASE_URL/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"call_id":"test-default-tos"}')
echo "   Response: $RESPONSE1"
SESSION1_ID=$(echo $RESPONSE1 | jq -r '.data.call_id')
PORT1=$(echo $RESPONSE1 | jq -r '.data.rtp_port')
echo "   ✓ Session created: $SESSION1_ID on port $PORT1"
echo ""

echo "2. Testing Voice TOS (EF: 0xB8/184)"
echo "   Creating session with TOS=184..."
RESPONSE2=$(curl -s -X POST $BASE_URL/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"call_id":"test-voice-ef", "tos":184}')
echo "   Response: $RESPONSE2"
SESSION2_ID=$(echo $RESPONSE2 | jq -r '.data.call_id')
PORT2=$(echo $RESPONSE2 | jq -r '.data.rtp_port')
echo "   ✓ Session created: $SESSION2_ID on port $PORT2"
echo ""

echo "3. Testing Video TOS (AF41: 0xA0/160)"
echo "   Creating session with TOS=160..."
RESPONSE3=$(curl -s -X POST $BASE_URL/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"call_id":"test-video-af41", "tos":160}')
echo "   Response: $RESPONSE3"
SESSION3_ID=$(echo $RESPONSE3 | jq -r '.data.call_id')
PORT3=$(echo $RESPONSE3 | jq -r '.data.rtp_port')
echo "   ✓ Session created: $SESSION3_ID on port $PORT3"
echo ""

echo "4. Testing Best Effort TOS (BE: 0x00/0)"
echo "   Creating session with TOS=0..."
RESPONSE4=$(curl -s -X POST $BASE_URL/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"call_id":"test-best-effort", "tos":0}')
echo "   Response: $RESPONSE4"
SESSION4_ID=$(echo $RESPONSE4 | jq -r '.data.call_id')
PORT4=$(echo $RESPONSE4 | jq -r '.data.rtp_port')
echo "   ✓ Session created: $SESSION4_ID on port $PORT4"
echo ""

echo "5. Listing all sessions..."
curl -s $BASE_URL/v1/sessions | jq '.'
echo ""

echo "========================================="
echo "TOS Configuration Summary"
echo "========================================="
echo "Session 1 (default):      port $PORT1  TOS: default (0xB8)"
echo "Session 2 (voice/EF):     port $PORT2  TOS: 0xB8 (184)"
echo "Session 3 (video/AF41):   port $PORT3  TOS: 0xA0 (160)"
echo "Session 4 (best effort):  port $PORT4  TOS: 0x00 (0)"
echo ""

echo "========================================="
echo "Verification Steps"
echo "========================================="
echo ""
echo "To verify TOS markings on actual packets:"
echo ""
echo "1. Start packet capture on port $PORT2 (voice/EF):"
echo "   sudo tcpdump -i any -n 'udp port $PORT2' -vvv | grep \"tos\""
echo ""
echo "2. Start packet capture on port $PORT3 (video/AF41):"
echo "   sudo tcpdump -i any -n 'udp port $PORT3' -vvv | grep \"tos\""
echo ""
echo "3. Check forge-media logs for TOS messages:"
echo "   grep -i \"custom TOS\" /path/to/forge-media.log"
echo ""
echo "4. Send test RTP packets to trigger forwarding:"
echo "   # Use a SIP phone or RTP generator to send packets"
echo ""

echo "========================================="
echo "Cleanup"
echo "========================================="
echo ""
echo "Delete test sessions:"
echo "curl -X DELETE $BASE_URL/v1/sessions/$SESSION1_ID"
echo "curl -X DELETE $BASE_URL/v1/sessions/$SESSION2_ID"
echo "curl -X DELETE $BASE_URL/v1/sessions/$SESSION3_ID"
echo "curl -X DELETE $BASE_URL/v1/sessions/$SESSION4_ID"
echo ""

read -p "Press Enter to delete test sessions..."

echo "Deleting sessions..."
curl -s -X DELETE $BASE_URL/v1/sessions/$SESSION1_ID && echo "  ✓ Deleted $SESSION1_ID"
curl -s -X DELETE $BASE_URL/v1/sessions/$SESSION2_ID && echo "  ✓ Deleted $SESSION2_ID"
curl -s -X DELETE $BASE_URL/v1/sessions/$SESSION3_ID && echo "  ✓ Deleted $SESSION3_ID"
curl -s -X DELETE $BASE_URL/v1/sessions/$SESSION4_ID && echo "  ✓ Deleted $SESSION4_ID"

echo ""
echo "========================================="
echo "Test Complete!"
echo "========================================="
