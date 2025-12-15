#!/bin/bash

# Forge Media - AI IVR Example
# This demonstrates an AI-powered IVR system with DTMF integration

set -e

BASE_URL="${FORGE_URL:-http://localhost:8080}"
CALL_ID="${CALL_ID:-ivr-call-$(date +%s)}"
OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-realtime-preview-2024-12-17}"

echo "============================================"
echo "Forge Media - AI IVR Demo"
echo "============================================"
echo ""

# Check OpenAI API key
if [ -z "$OPENAI_API_KEY" ]; then
    echo "ERROR: OPENAI_API_KEY not set"
    exit 1
fi

echo "Step 1: Create session for IVR"
echo "-------------------------------"
curl -s -X POST "$BASE_URL/v1/sessions" \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "'"$CALL_ID"'"
  }' | jq '.'

echo ""
echo "Step 2: Attach AI with IVR instructions"
echo "----------------------------------------"
curl -s -X POST "$BASE_URL/v1/sessions/$CALL_ID/ai" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "'"$OPENAI_MODEL"'",
    "voice": "alloy",
    "instructions": "You are an automated phone menu system (IVR). When the call starts, greet the caller and say: '\''Thank you for calling. Please press 1 for Sales, 2 for Support, or 3 for Billing.'\'' When you receive a DTMF event showing which key the user pressed, acknowledge their selection and explain what would happen next. For example, if they press 1, say '\''You selected Sales. Transferring you now...'\'' Be concise and speak naturally like an IVR system.",
    "temperature": 0.3,
    "turn_detection": {
      "type": "server_vad",
      "threshold": 0.6,
      "silence_duration_ms": 1000
    }
  }' | jq '.'

echo ""
echo "============================================"
echo "AI IVR Active!"
echo "============================================"
echo ""
echo "Call ID: $CALL_ID"
echo ""
echo "The IVR will:"
echo "  1. Greet the caller"
echo "  2. Present menu options (Press 1/2/3)"
echo "  3. Detect DTMF tones automatically"
echo "  4. Respond based on the pressed key"
echo ""
echo "DTMF events are forwarded to OpenAI as text:"
echo "  '[DTMF: User pressed 1 via rfc2833]'"
echo ""
echo "This allows the AI to understand user input and"
echo "respond appropriately without custom programming."
echo ""
echo "Monitor status:"
echo "  curl $BASE_URL/v1/sessions/$CALL_ID/ai | jq .stats"
echo ""
