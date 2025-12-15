#!/usr/bin/env python3
"""
Simple HTTP server for serving WebRTC browser tests.

Usage:
    python3 serve.py [port]

Default port: 8000

Then open: http://localhost:8000/webrtc-test.html
"""

import http.server
import socketserver
import sys
import os

# Default port
PORT = 8000

# Parse command line arguments
if len(sys.argv) > 1:
    try:
        PORT = int(sys.argv[1])
    except ValueError:
        print(f"Invalid port: {sys.argv[1]}")
        print("Usage: python3 serve.py [port]")
        sys.exit(1)

# Change to script directory
os.chdir(os.path.dirname(os.path.abspath(__file__)))

# Create handler with CORS headers for WebRTC testing
class CORSRequestHandler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        # Add CORS headers
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', 'Content-Type')
        # Cache control for development
        self.send_header('Cache-Control', 'no-store, no-cache, must-revalidate')
        super().end_headers()

    def do_OPTIONS(self):
        self.send_response(200)
        self.end_headers()

    def log_message(self, format, *args):
        # Custom log format with colors
        if args[1] == '200':
            print(f"\033[92m[✓]\033[0m {args[0]} - {args[1]}")
        elif args[1].startswith('4') or args[1].startswith('5'):
            print(f"\033[91m[✗]\033[0m {args[0]} - {args[1]}")
        else:
            print(f"[→] {args[0]} - {args[1]}")

# Create server
Handler = CORSRequestHandler

try:
    with socketserver.TCPServer(("", PORT), Handler) as httpd:
        print("=" * 60)
        print(f"🚀 WebRTC Test Server")
        print("=" * 60)
        print(f"Server running at: http://localhost:{PORT}/")
        print(f"\nOpen test page: http://localhost:{PORT}/webrtc-test.html")
        print("\nMake sure Forge Media server is running on:")
        print("  http://localhost:8080 (or update API URL in test page)")
        print("\nPress Ctrl+C to stop")
        print("=" * 60)
        print()
        httpd.serve_forever()
except KeyboardInterrupt:
    print("\n\n👋 Server stopped")
    sys.exit(0)
except OSError as e:
    if e.errno == 48 or e.errno == 98:  # Address already in use
        print(f"\n❌ Error: Port {PORT} is already in use")
        print(f"Try a different port: python3 serve.py 8001")
        sys.exit(1)
    else:
        raise
