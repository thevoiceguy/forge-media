#!/bin/bash

# Forge Media - AI Function Calling Example
# Demonstrates how AI can trigger actions via function calling

set -e

BASE_URL="${FORGE_URL:-http://localhost:8080}"
CALL_ID="${CALL_ID:-func-call-$(date +%s)}"
OPENAI_MODEL="${OPENAI_MODEL:-gpt-4o-realtime-preview-2024-12-17}"

echo "============================================"
echo "Forge Media - AI Function Calling Demo"
echo "============================================"
echo ""

if [ -z "$OPENAI_API_KEY" ]; then
    echo "ERROR: OPENAI_API_KEY not set"
    exit 1
fi

echo "Step 1: Create session"
echo "----------------------"
curl -s -X POST "$BASE_URL/v1/sessions" \
  -H "Content-Type: application/json" \
  -d '{
    "call_id": "'"$CALL_ID"'"
  }' | jq '.'

echo ""
echo "Step 2: Attach AI with function tools"
echo "--------------------------------------"
curl -s -X POST "$BASE_URL/v1/sessions/$CALL_ID/ai" \
  -H "Content-Type: application/json" \
  -d '{
    "provider": "openai",
    "model": "'"$OPENAI_MODEL"'",
    "voice": "shimmer",
    "instructions": "You are a customer service agent. You can help users check their account balance, transfer calls to different departments, or schedule callbacks. Use the provided functions when appropriate.",
    "temperature": 0.7,
    "tools": [
      {
        "type": "function",
        "name": "get_account_balance",
        "description": "Retrieve the current account balance for a customer",
        "parameters": {
          "type": "object",
          "properties": {
            "account_number": {
              "type": "string",
              "description": "The customer account number"
            }
          },
          "required": ["account_number"]
        }
      },
      {
        "type": "function",
        "name": "transfer_call",
        "description": "Transfer the call to another department",
        "parameters": {
          "type": "object",
          "properties": {
            "department": {
              "type": "string",
              "enum": ["sales", "support", "billing", "technical"],
              "description": "The department to transfer to"
            },
            "reason": {
              "type": "string",
              "description": "Reason for transfer"
            }
          },
          "required": ["department"]
        }
      },
      {
        "type": "function",
        "name": "schedule_callback",
        "description": "Schedule a callback for the customer",
        "parameters": {
          "type": "object",
          "properties": {
            "phone_number": {
              "type": "string",
              "description": "Phone number to call back"
            },
            "preferred_time": {
              "type": "string",
              "description": "Preferred callback time"
            },
            "topic": {
              "type": "string",
              "description": "Topic for the callback"
            }
          },
          "required": ["phone_number", "preferred_time"]
        }
      }
    ]
  }' | jq '.'

echo ""
echo "============================================"
echo "AI with Function Calling Active!"
echo "============================================"
echo ""
echo "Call ID: $CALL_ID"
echo ""
echo "The AI can now call these functions:"
echo "  • get_account_balance(account_number)"
echo "  • transfer_call(department, reason)"
echo "  • schedule_callback(phone_number, preferred_time, topic)"
echo ""
echo "When the AI calls a function, you'll receive an event"
echo "on the EventBus. Your application should:"
echo ""
echo "  1. Execute the function (query DB, initiate transfer, etc.)"
echo "  2. Send the result back to OpenAI:"
echo ""
echo "Example function response:"
echo "  curl -X POST $BASE_URL/v1/sessions/$CALL_ID/ai/function-response \\"
echo "    -H 'Content-Type: application/json' \\"
echo "    -d '{"
echo "      \"call_id\": \"fc-12345\","
echo "      \"output\": \"{\\\"balance\\\": \\\"$245.67\\\", \\\"status\\\": \\\"active\\\"}\""
echo "    }'"
echo ""
echo "The AI will then use this information in its response:"
echo "  'Your current balance is $245.67...'"
echo ""
echo "Monitor for function calls in your application logs or"
echo "subscribe to the EventBus for AIEvent::FunctionCall events."
echo ""
