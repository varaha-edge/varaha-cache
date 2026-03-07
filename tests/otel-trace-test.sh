#!/usr/bin/env bash
#
# otel-trace-test.sh
#
# End-to-end test for varaha-cache OpenTelemetry trace export.
#
# What it does:
#   1. Starts Jaeger (OTLP receiver + trace UI) in Docker
#   2. Starts three backend HTTP servers (static, api, images)
#   3. Writes a full-featured VCL with routing, TTLs, headers, grace, pass, purge, synth
#   4. Starts varaha-cache pointing at local Jaeger
#   5. Sends mixed traffic continuously for the specified duration
#   6. Prints a trace summary report from Jaeger
#   7. Cleans up all processes
#
# Requirements:
#   - Docker (for Jaeger)
#   - cargo (varaha-cache built from source)
#   - curl, python3 (for backends and traffic generation)
#

set -euo pipefail

# -------------------------------------------------------------------
# Defaults
# -------------------------------------------------------------------
DURATION_SECONDS=300
JAEGER_OTLP_PORT=4317
JAEGER_UI_PORT=16686
BACKEND_STATIC_PORT=8201
BACKEND_API_PORT=8202
BACKEND_IMAGES_PORT=8203
CACHE_HTTP_PORT=6081
CACHE_ADMIN_PORT=6082
CACHE_STORAGE="malloc,128m"
SKIP_BUILD=false

# -------------------------------------------------------------------
# Usage
# -------------------------------------------------------------------
usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  -d, --duration SECONDS     Test duration in seconds (default: 300)
  -j, --jaeger-otlp PORT     Jaeger OTLP gRPC port (default: 4317)
  -u, --jaeger-ui PORT       Jaeger UI port (default: 16686)
  -p, --cache-port PORT      varaha-cache HTTP listen port (default: 6081)
  -a, --admin-port PORT      varaha-cache admin port (default: 6082)
  -s, --storage SPEC         Cache storage spec (default: malloc,128m)
      --backend-static PORT  Static backend port (default: 8201)
      --backend-api PORT     API backend port (default: 8202)
      --backend-images PORT  Images backend port (default: 8203)
      --skip-build           Skip cargo build (use existing binary)
  -h, --help                 Show this help

Examples:
  $(basename "$0")                          # 5-minute test, default ports
  $(basename "$0") -d 30                    # 30-second quick test
  $(basename "$0") -d 60 -j 4318 -u 16687  # 1-minute test, alternate Jaeger ports
  $(basename "$0") --skip-build -d 30       # Quick test, skip rebuild
EOF
    exit 0
}

# -------------------------------------------------------------------
# Parse CLI options
# -------------------------------------------------------------------
while [ $# -gt 0 ]; do
    case "$1" in
        -d|--duration)
            DURATION_SECONDS="$2"; shift 2 ;;
        -j|--jaeger-otlp)
            JAEGER_OTLP_PORT="$2"; shift 2 ;;
        -u|--jaeger-ui)
            JAEGER_UI_PORT="$2"; shift 2 ;;
        -p|--cache-port)
            CACHE_HTTP_PORT="$2"; shift 2 ;;
        -a|--admin-port)
            CACHE_ADMIN_PORT="$2"; shift 2 ;;
        -s|--storage)
            CACHE_STORAGE="$2"; shift 2 ;;
        --backend-static)
            BACKEND_STATIC_PORT="$2"; shift 2 ;;
        --backend-api)
            BACKEND_API_PORT="$2"; shift 2 ;;
        --backend-images)
            BACKEND_IMAGES_PORT="$2"; shift 2 ;;
        --skip-build)
            SKIP_BUILD=true; shift ;;
        -h|--help)
            usage ;;
        *)
            echo "Unknown option: $1"
            usage ;;
    esac
done

# -------------------------------------------------------------------
# Derived values
# -------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PIDS=()

cleanup() {
    echo ""
    echo "--- Cleaning up ---"
    if [ ${#PIDS[@]} -gt 0 ]; then
        for pid in "${PIDS[@]}"; do
            kill "$pid" 2>/dev/null || true
        done
    fi
    wait 2>/dev/null || true
    docker rm -f jaeger-otel-test 2>/dev/null || true
    rm -f /tmp/otel-test-*.vcl /tmp/otel-test-*.py /tmp/otel-test.vcl
    echo "Cleanup complete."
}
trap cleanup EXIT

echo "============================================="
echo " varaha-cache OTEL Trace Integration Test"
echo "============================================="
echo ""
echo "  Duration:       ${DURATION_SECONDS}s ($((DURATION_SECONDS / 60))m)"
echo "  Jaeger OTLP:    :${JAEGER_OTLP_PORT}"
echo "  Jaeger UI:      :${JAEGER_UI_PORT}"
echo "  Cache HTTP:     :${CACHE_HTTP_PORT}"
echo "  Cache Admin:    :${CACHE_ADMIN_PORT}"
echo "  Cache Storage:  ${CACHE_STORAGE}"
echo "  Backends:       :${BACKEND_STATIC_PORT} :${BACKEND_API_PORT} :${BACKEND_IMAGES_PORT}"
echo ""

# -------------------------------------------------------------------
# Step 1: Start Jaeger
# -------------------------------------------------------------------
echo "[1/6] Starting Jaeger (OTLP on :$JAEGER_OTLP_PORT, UI on :$JAEGER_UI_PORT)..."
docker rm -f jaeger-otel-test 2>/dev/null || true
sleep 1
docker run -d --name jaeger-otel-test \
    -p $JAEGER_OTLP_PORT:4317 \
    -p $JAEGER_UI_PORT:16686 \
    jaegertracing/all-in-one:latest >/dev/null 2>&1

# Wait for Jaeger to be ready (retry up to 10 times)
JAEGER_READY=false
for i in $(seq 1 10); do
    if curl -sf http://localhost:$JAEGER_UI_PORT/ >/dev/null 2>&1; then
        JAEGER_READY=true
        break
    fi
    sleep 1
done

if [ "$JAEGER_READY" = "false" ]; then
    echo "ERROR: Jaeger failed to start. Check Docker."
    docker logs jaeger-otel-test 2>&1 | tail -5
    exit 1
fi
echo "  Jaeger running. UI at http://localhost:$JAEGER_UI_PORT"

# -------------------------------------------------------------------
# Step 2: Start backend servers
# -------------------------------------------------------------------
echo "[2/6] Starting backend servers..."

# Backend 1: Static content server
cat > /tmp/otel-test-static.py << PYEOF
from http.server import HTTPServer, BaseHTTPRequestHandler
import time, random

PAGES = {
    "/": "Welcome to varaha-cache test site\n",
    "/about": "About page - varaha-cache is a high-performance HTTP cache\n",
    "/contact": "Contact us at hello@varaha.io\n",
    "/products": "Product catalog: item-1, item-2, item-3, item-4, item-5\n",
    "/docs": "Documentation: getting started, configuration, VCL reference\n",
    "/blog/post-1": "Blog post 1: Introduction to caching\n",
    "/blog/post-2": "Blog post 2: VCL programming guide\n",
    "/blog/post-3": "Blog post 3: Performance tuning tips\n",
    "/faq": "FAQ: How does caching work? What is TTL? What is grace mode?\n",
    "/status": "OK\n",
}

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(random.uniform(0.001, 0.02))
        body = PAGES.get(self.path, f"Static page: {self.path}\n")
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("X-Backend", "static")
        self.send_header("Cache-Control", "public, max-age=60")
        self.send_header("ETag", f'"static-{hash(self.path) % 10000}"')
        self.end_headers()
        self.wfile.write(body.encode())
    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("X-Backend", "static")
        self.end_headers()
    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", $BACKEND_STATIC_PORT), Handler).serve_forever()
PYEOF
python3 /tmp/otel-test-static.py &
PIDS+=($!)

# Backend 2: API server - slower, uncacheable responses
cat > /tmp/otel-test-api.py << PYEOF
from http.server import HTTPServer, BaseHTTPRequestHandler
import time, random, json

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(random.uniform(0.005, 0.05))
        data = {
            "path": self.path,
            "timestamp": time.time(),
            "random": random.randint(1, 1000),
            "server": "api-backend",
        }
        body = json.dumps(data) + "\n"
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("X-Backend", "api")
        self.send_header("Cache-Control", "no-store, no-cache")
        self.end_headers()
        self.wfile.write(body.encode())
    def do_POST(self):
        content_len = int(self.headers.get("Content-Length", 0))
        self.rfile.read(content_len)
        time.sleep(random.uniform(0.01, 0.03))
        self.send_response(201)
        self.send_header("Content-Type", "application/json")
        self.send_header("X-Backend", "api")
        self.end_headers()
        self.wfile.write(b'{"status":"created"}\n')
    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", $BACKEND_API_PORT), Handler).serve_forever()
PYEOF
python3 /tmp/otel-test-api.py &
PIDS+=($!)

# Backend 3: Image/asset server - large responses, long TTL
cat > /tmp/otel-test-images.py << PYEOF
from http.server import HTTPServer, BaseHTTPRequestHandler
import time, random

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(random.uniform(0.002, 0.01))
        if "large" in self.path:
            size = random.randint(50000, 100000)
        elif "medium" in self.path:
            size = random.randint(10000, 50000)
        else:
            size = random.randint(1000, 10000)
        body = b"X" * size
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("X-Backend", "images")
        self.send_header("Cache-Control", "public, max-age=86400")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", $BACKEND_IMAGES_PORT), Handler).serve_forever()
PYEOF
python3 /tmp/otel-test-images.py &
PIDS+=($!)

sleep 1

# Verify backends
for port in $BACKEND_STATIC_PORT $BACKEND_API_PORT $BACKEND_IMAGES_PORT; do
    if curl -sf http://127.0.0.1:$port/ >/dev/null 2>&1; then
        echo "  Backend on :$port OK"
    else
        echo "  ERROR: Backend on :$port failed to start"
        exit 1
    fi
done

# -------------------------------------------------------------------
# Step 3: Write test VCL
# -------------------------------------------------------------------
echo "[3/6] Writing test VCL..."

cat > /tmp/otel-test.vcl << VCLEOF
vcl 4.0;

backend default {
    .host = "127.0.0.1";
    .port = "$BACKEND_STATIC_PORT";
}

sub vcl_recv {
    # Health check endpoint - synthetic response, no backend
    if (req.url == "/healthz") {
        return(synth(200, "OK"));
    }

    # Purge support
    if (req.method == "PURGE") {
        return(purge);
    }

    # API requests - always pass (uncacheable)
    if (req.url ~ "^/api/") {
        return(pass);
    }

    # POST requests - pass through
    if (req.method == "POST") {
        return(pass);
    }

    # Strip tracking query parameters for better cache hit rate
    set req.url = regsuball(req.url, "[?&](utm_[a-z]+|fbclid|gclid)=[^&]*", "");
    # Clean up leading ? if all params were stripped
    set req.url = regsub(req.url, "\?$", "");

    # Normalize Accept-Encoding for better cache hit rate
    if (req.http.Accept-Encoding) {
        if (req.http.Accept-Encoding ~ "gzip") {
            set req.http.Accept-Encoding = "gzip";
        } else {
            unset req.http.Accept-Encoding;
        }
    }

    # Default: look up in cache
    return(hash);
}

sub vcl_backend_response {
    # Set default TTL if backend did not specify
    if (beresp.status == 200) {
        set beresp.ttl = 120s;
        set beresp.grace = 30s;
    }

    # Long TTL for static assets
    if (bereq.url ~ "\.(css|js|png|jpg|gif|ico|woff|svg)$") {
        set beresp.ttl = 3600s;
        set beresp.grace = 60s;
    }

    # Short TTL for dynamic-ish pages
    if (bereq.url ~ "^/blog/") {
        set beresp.ttl = 60s;
        set beresp.grace = 120s;
    }

    # Do not cache error responses
    if (beresp.status >= 400) {
        set beresp.uncacheable = true;
        set beresp.ttl = 0s;
    }

    return(deliver);
}

sub vcl_deliver {
    # X-Cache header is already set by the runtime (HIT/MISS/SYNTH/ERROR).
    # Add hit count as an extra header for debugging.
    set resp.http.X-Cache-Hits = obj.hits;

    # Remove internal headers before sending to client
    unset resp.http.X-Backend;

    return(deliver);
}

sub vcl_synth {
    if (resp.status == 200) {
        synthetic("healthy");
    }
    return(deliver);
}
VCLEOF
echo "  VCL written to /tmp/otel-test.vcl"

# -------------------------------------------------------------------
# Step 4: Build and start varaha-cache
# -------------------------------------------------------------------
echo "[4/6] Building and starting varaha-cache..."
cd "$PROJECT_DIR"
if [ "$SKIP_BUILD" = "false" ]; then
    cargo build --bin varaha-cache --release 2>&1 | tail -3
else
    echo "  Skipping build (--skip-build)"
fi

RUST_LOG=info \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:$JAEGER_OTLP_PORT \
OTEL_SERVICE_NAME=varaha-cache \
./target/release/varaha-cache \
    -a 127.0.0.1:$CACHE_HTTP_PORT \
    -T 127.0.0.1:$CACHE_ADMIN_PORT \
    -f /tmp/otel-test.vcl \
    -s "$CACHE_STORAGE" \
    &
PIDS+=($!)
sleep 2

# Verify cache is up
if curl -sf http://127.0.0.1:$CACHE_HTTP_PORT/healthz >/dev/null 2>&1; then
    echo "  varaha-cache running on :$CACHE_HTTP_PORT (admin :$CACHE_ADMIN_PORT)"
else
    echo "  ERROR: varaha-cache failed to start"
    exit 1
fi

# -------------------------------------------------------------------
# Step 5: Send traffic
# -------------------------------------------------------------------
DURATION_DISPLAY="$((DURATION_SECONDS / 60))m $((DURATION_SECONDS % 60))s"
echo "[5/6] Sending traffic for ${DURATION_DISPLAY}..."
echo "  Jaeger UI: http://localhost:$JAEGER_UI_PORT (select service: varaha-cache)"
echo ""

# URL pools for different traffic patterns
STATIC_URLS=(
    "/"
    "/about"
    "/contact"
    "/products"
    "/docs"
    "/blog/post-1"
    "/blog/post-2"
    "/blog/post-3"
    "/faq"
    "/status"
)

ASSET_URLS=(
    "/assets/style.css"
    "/assets/app.js"
    "/assets/logo.png"
    "/assets/hero-large.jpg"
    "/assets/icon-medium.gif"
    "/assets/font.woff"
    "/assets/sprite.svg"
    "/assets/favicon.ico"
)

API_URLS=(
    "/api/users"
    "/api/products"
    "/api/orders"
    "/api/search?q=cache"
    "/api/stats"
)

UNIQUE_COUNTER=0

# Counters
TOTAL=0
HITS=0
MISSES=0
PASSES=0
ERRORS=0
SYNTHS=0
PURGES=0

START_TIME=$(date +%s)
END_TIME=$((START_TIME + DURATION_SECONDS))
LAST_REPORT=$START_TIME

echo "  Time     | Total   | Hits    | Misses  | Pass    | Synth   | Purge   | Errors  | RPS"
echo "  ---------|---------|---------|---------|---------|---------|---------|---------|-----"

# Helper: send request and return "status_code|X-Cache-value"
send_req() {
    local EXTRA_ARGS=("$@")
    local HEADERS
    HEADERS=$(curl -s -D - -o /dev/null "${EXTRA_ARGS[@]}" 2>/dev/null) || echo "HTTP/1.1 000"
    local CODE
    CODE=$(echo "$HEADERS" | head -1 | awk '{print $2}')
    local XCACHE
    XCACHE=$(echo "$HEADERS" | grep -i "^X-Cache:" | head -1 | awk '{print $2}' | tr -d '\r\n ')
    echo "${CODE:-000}|${XCACHE}"
}

while [ "$(date +%s)" -lt "$END_TIME" ]; do
    # Pick a random traffic pattern (weighted)
    RAND=$((RANDOM % 100))

    if [ $RAND -lt 40 ]; then
        # 40% - Cacheable static pages (high hit rate expected)
        IDX=$((RANDOM % ${#STATIC_URLS[@]}))
        URL="${STATIC_URLS[$IDX]}"
        RESPONSE=$(send_req http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    elif [ $RAND -lt 55 ]; then
        # 15% - Static assets (long TTL, high hit rate)
        IDX=$((RANDOM % ${#ASSET_URLS[@]}))
        URL="${ASSET_URLS[$IDX]}"
        RESPONSE=$(send_req http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    elif [ $RAND -lt 70 ]; then
        # 15% - API requests (always pass, never cached)
        IDX=$((RANDOM % ${#API_URLS[@]}))
        URL="${API_URLS[$IDX]}"
        RESPONSE=$(send_req http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    elif [ $RAND -lt 80 ]; then
        # 10% - Unique URLs (always cache miss on first hit)
        UNIQUE_COUNTER=$((UNIQUE_COUNTER + 1))
        URL="/unique/page-$UNIQUE_COUNTER"
        RESPONSE=$(send_req http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    elif [ $RAND -lt 85 ]; then
        # 5% - URLs with tracking params (should be stripped by VCL)
        IDX=$((RANDOM % ${#STATIC_URLS[@]}))
        URL="${STATIC_URLS[$IDX]}?utm_source=test&utm_medium=cli&fbclid=abc123"
        RESPONSE=$(send_req http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    elif [ $RAND -lt 90 ]; then
        # 5% - Health check (synthetic, no backend)
        URL="/healthz"
        RESPONSE=$(send_req http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    elif [ $RAND -lt 95 ]; then
        # 5% - Conditional requests (If-None-Match)
        IDX=$((RANDOM % ${#STATIC_URLS[@]}))
        URL="${STATIC_URLS[$IDX]}"
        RESPONSE=$(send_req -H "If-None-Match: \"static-$((RANDOM % 10000))\"" http://127.0.0.1:$CACHE_HTTP_PORT"$URL")

    else
        # 5% - Purge requests
        IDX=$((RANDOM % ${#STATIC_URLS[@]}))
        URL="${STATIC_URLS[$IDX]}"
        RESPONSE=$(send_req -X PURGE http://127.0.0.1:$CACHE_HTTP_PORT"$URL")
    fi

    # Parse response
    HTTP_CODE="${RESPONSE%%|*}"
    CACHE_STATUS="${RESPONSE##*|}"

    TOTAL=$((TOTAL + 1))

    case "$CACHE_STATUS" in
        HIT)    HITS=$((HITS + 1)) ;;
        MISS)   MISSES=$((MISSES + 1)) ;;
        SYNTH)  SYNTHS=$((SYNTHS + 1)) ;;
        ERROR)  ERRORS=$((ERRORS + 1)) ;;
        "")
            # No X-Cache header: synthetic response, pass, or purge
            if [ "$RAND" -ge 95 ]; then
                PURGES=$((PURGES + 1))
            elif [ "$RAND" -ge 85 ]; then
                SYNTHS=$((SYNTHS + 1))
            else
                PASSES=$((PASSES + 1))
            fi
            ;;
        *)      PASSES=$((PASSES + 1)) ;;
    esac

    if [ "$HTTP_CODE" = "000" ] || [ "${HTTP_CODE:-0}" -ge 500 ] 2>/dev/null; then
        ERRORS=$((ERRORS + 1))
    fi

    # Print progress every 10 seconds
    NOW=$(date +%s)
    ELAPSED=$((NOW - START_TIME))
    if [ $((NOW - LAST_REPORT)) -ge 10 ]; then
        RPS=$((TOTAL / (ELAPSED > 0 ? ELAPSED : 1)))
        printf "  %3dm %02ds | %7d | %7d | %7d | %7d | %7d | %7d | %7d | %d\n" \
            $((ELAPSED / 60)) $((ELAPSED % 60)) \
            $TOTAL $HITS $MISSES $PASSES $SYNTHS $PURGES $ERRORS $RPS
        LAST_REPORT=$NOW
    fi

    # Small random delay between requests
    read -t 0.02 -r _ 2>/dev/null || true
done

ELAPSED=$(($(date +%s) - START_TIME))
RPS=$((TOTAL / (ELAPSED > 0 ? ELAPSED : 1)))

echo "  ---------|---------|---------|---------|---------|---------|---------|---------|-----"
printf "  TOTAL    | %7d | %7d | %7d | %7d | %7d | %7d | %7d | %d\n" \
    $TOTAL $HITS $MISSES $PASSES $SYNTHS $PURGES $ERRORS $RPS
echo ""

if [ $TOTAL -gt 0 ]; then
    HIT_RATE=$(( (HITS * 100) / TOTAL ))
    echo "  Cache hit rate: ${HIT_RATE}%"
    echo "  Error rate: $(( (ERRORS * 100) / TOTAL ))%"
fi
echo ""

# Wait for OTEL batch export to flush
echo "  Waiting for trace export to flush..."
sleep 8

# -------------------------------------------------------------------
# Step 6: Print trace report from Jaeger
# -------------------------------------------------------------------
echo "[6/6] Trace report from Jaeger..."
echo ""

curl -s "http://localhost:$JAEGER_UI_PORT/api/traces?service=varaha-cache&limit=200&lookback=10m" | python3 -c "
import json, sys

data = json.load(sys.stdin)
traces = data.get('data', [])

total = len(traces)
if total == 0:
    print('  No traces found in Jaeger.')
    sys.exit(0)

# Analyze traces
hits = 0
misses = 0
fetches = 0
errors = 0
durations = []
fetch_durations = []
span_counts = {2: 0, 3: 0}

for t in traces:
    spans = t.get('spans', [])
    span_counts[len(spans)] = span_counts.get(len(spans), 0) + 1

    for s in spans:
        tags = {tag['key']: tag['value'] for tag in s.get('tags', [])}

        if s['operationName'] == 'handle_request':
            dur_us = s.get('duration', 0)
            durations.append(dur_us)
            if tags.get('cache.hit') == True:
                hits += 1
            else:
                misses += 1
            status = tags.get('http.status_code', 0)
            if isinstance(status, int) and status >= 500:
                errors += 1

        if s['operationName'] == 'backend_fetch':
            fetches += 1
            fetch_durations.append(s.get('duration', 0))

durations.sort()
fetch_durations.sort()

def percentile(arr, p):
    if not arr:
        return 0
    idx = int(len(arr) * p / 100)
    return arr[min(idx, len(arr) - 1)]

print(f'  Traces collected:      {total}')
print(f'  Cache hits:            {hits}')
print(f'  Cache misses:          {misses}')
print(f'  Backend fetches:       {fetches}')
print(f'  Server errors (5xx):   {errors}')
print()
print(f'  Request latency:')
print(f'    p50:  {percentile(durations, 50):>8} us')
print(f'    p90:  {percentile(durations, 90):>8} us')
print(f'    p99:  {percentile(durations, 99):>8} us')
print(f'    max:  {max(durations) if durations else 0:>8} us')
print()
if fetch_durations:
    print(f'  Backend fetch latency:')
    print(f'    p50:  {percentile(fetch_durations, 50):>8} us')
    print(f'    p90:  {percentile(fetch_durations, 90):>8} us')
    print(f'    p99:  {percentile(fetch_durations, 99):>8} us')
    print(f'    max:  {max(fetch_durations):>8} us')
    print()

print(f'  Span distribution:')
for k in sorted(span_counts.keys()):
    label = 'cache hit (no fetch)' if k == 2 else 'cache miss (with fetch)' if k == 3 else f'{k} spans'
    print(f'    {k} spans: {span_counts[k]:>5}  ({label})')

print()
print(f'  Unique operations:')
ops = {}
for t in traces:
    for s in t.get('spans', []):
        name = s['operationName']
        ops[name] = ops.get(name, 0) + 1
for name, count in sorted(ops.items(), key=lambda x: -x[1]):
    print(f'    {name:.<30} {count:>5}')
" 2>&1

echo ""
echo "============================================="
echo " Test complete."
echo " Jaeger UI: http://localhost:$JAEGER_UI_PORT"
echo " Select service 'varaha-cache' to browse traces."
echo ""
echo " To stop Jaeger:  docker rm -f jaeger-otel-test"
echo "============================================="
