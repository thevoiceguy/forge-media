#!/bin/bash

# Forge Media - AI Integration Example
# This script demonstrates how to integrate OpenAI's Realtime API with Forge Media

set -e

# Configuration
BASE_URL="${FORGE_URL:-http://localhost:8080}"
CALL_ID="${CALL_ID:-demo-call-001}"
OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-realtime-preview-2024-12-17}"
OPENAI_VOICE="${OPENAI_VOICE:-alloy}"

echo "============================================"
echo "Forge Media - AI Integration Demo"
echo "============================================"
echo ""

# Check if OpenAI API key is set
if [ -z "$OPENAI_API_KEY" ]; then
    echo "ERROR: OPENAI_API_KEY environment variable not set"
    echo "Usage: export OPENAI_API_KEY='your-api-key-here'"
    exit 1
fi

echo "Step 1: Create a media session"
echo "--------------------------------"
curl -s -X POST "$BASE_URL/v1/sessions" \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "'"$CALL_ID"'",
    "sdp": "v=0\r\no=- 0 0 IN IP4 192.168.1.100\r\ns=Forge Media Session\r\nc=IN IP4 192.168.1.100\r\nt=0 0\r\nm=audio 50000 RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-16\r\na=sendrecv\r\n"
  }' | jq '.'

echo ""
echo "Step 2: Attach AI to the session"
echo "---------------------------------"
AI_RESPONSE=$(curl -s -X POST "$BASE_URL/v1/sessions/$CALL_ID/ai" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "'"$OPENAI_MODEL"'",
    "voice": "'"$OPENAI_VOICE"'",
    "instructions": "You are a friendly customer service agent for a telecommunications company. Help users with their questions about their service, billing, and technical issues. Be concise and professional.",
    "temperature": 0.8,
    "turn_detection": {
      "type": "server_vad",
      "threshold": 0.5,
      "prefix_padding_ms": 300,
      "silence_duration_ms": 500
    }
  }')

echo "$AI_RESPONSE" | jq '.'

echo ""
echo "Step 3: Check AI session status"
echo "--------------------------------"
sleep 2
curl -s "$BASE_URL/v1/sessions/$CALL_ID/ai" | jq '.'

echo ""
echo "============================================"
echo "AI Integration Active!"
echo "============================================"
echo ""
echo "The call is now connected to OpenAI's Realtime API."
echo "Audio from participants will be sent to the AI, and"
echo "AI responses will be sent back to participants."
echo ""
echo "DTMF events will also be forwarded to the AI."
echo ""
echo "To monitor the session:"
echo "  curl $BASE_URL/v1/sessions/$CALL_ID/ai"
echo ""
echo "To detach AI:"
echo "  curl -X DELETE $BASE_URL/v1/sessions/$CALL_ID/ai"
echo ""
