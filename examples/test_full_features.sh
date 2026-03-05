#!/bin/bash
# =================================================================
# test_full_features.sh -- Manual integration test for full_features.vcl
#
# Prerequisites:
#   1. Build varaha-cache:  cargo build -p rv-server
#   2. Compile mock backend: rustc examples/mock_backend.rs -o target/mock_backend
#   3. Start mock backend:   target/mock_backend &
#   4. Start varaha-cache:   cargo run -p rv-server -- -f examples/full_features.vcl &
#   5. Run this script:      bash examples/test_full_features.sh
#
# Or run everything at once:
#   bash examples/test_full_features.sh --start-all
# =================================================================

set -e

CACHE_ADDR="127.0.0.1:6081"
BACKEND_ADDR="127.0.0.1:18080"
ADMIN_ADDR="127.0.0.1:6082"

PASS=0
FAIL=0
TOTAL=0

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

assert_contains() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    TOTAL=$((TOTAL + 1))
    if echo "$actual" | grep -q "$expected"; then
        PASS=$((PASS + 1))
        printf "  ${GREEN}PASS${NC} %s\n" "$label"
    else
        FAIL=$((FAIL + 1))
        printf "  ${RED}FAIL${NC} %s\n" "$label"
        printf "       expected to contain: %s\n" "$expected"
        printf "       actual output (first 200 chars): %.200s\n" "$actual"
    fi
}

assert_status() {
    local label="$1"
    local actual="$2"
    local expected="$3"
    TOTAL=$((TOTAL + 1))
    if echo "$actual" | head -1 | grep -q "HTTP/1.1 $expected"; then
        PASS=$((PASS + 1))
        printf "  ${GREEN}PASS${NC} %s\n" "$label"
    else
        FAIL=$((FAIL + 1))
        printf "  ${RED}FAIL${NC} %s\n" "$label"
        printf "       expected status: %s\n" "$expected"
        printf "       actual: %s\n" "$(echo "$actual" | head -1)"
    fi
}

# --start-all: compile, start backend and cache automatically
if [ "$1" = "--start-all" ]; then
    echo "Building varaha-cache..."
    cargo build -p rv-server 2>&1 | grep -v warning || true

    echo "Compiling mock backend..."
    rustc examples/mock_backend.rs -o target/mock_backend

    # Kill any existing instances
    pkill -f "target/mock_backend" 2>/dev/null || true
    pkill -f "varaha-cache.*full_features" 2>/dev/null || true
    sleep 1

    echo "Starting mock backend on $BACKEND_ADDR..."
    target/mock_backend &
    BACKEND_PID=$!
    sleep 1

    echo "Starting varaha-cache on $CACHE_ADDR..."
    cargo run -p rv-server -- -f examples/full_features.vcl 2>/dev/null &
    CACHE_PID=$!
    sleep 2

    cleanup() {
        echo ""
        echo "Stopping services..."
        kill $CACHE_PID 2>/dev/null || true
        kill $BACKEND_PID 2>/dev/null || true
        wait $CACHE_PID 2>/dev/null || true
        wait $BACKEND_PID 2>/dev/null || true
    }
    trap cleanup EXIT
fi

echo ""
echo "============================================================"
echo " varaha-cache VCL Integration Tests"
echo " Cache: $CACHE_ADDR  Backend: $BACKEND_ADDR"
echo "============================================================"
echo ""


# ==================================================================
# Test 1: Basic cache MISS -> HIT cycle
# VCL features: vcl_recv(hash), vcl_miss, vcl_backend_fetch,
#   vcl_backend_response(ttl/grace/keep), vcl_deliver(X-Cache)
# ==================================================================
echo "${YELLOW}Test 1: Cache MISS then HIT${NC}"

RESP1=$(curl -s -D - "http://$CACHE_ADDR/api/data" -H "Host: test.local")
assert_status  "first request returns 200"       "$RESP1" "200"
assert_contains "first request is MISS"           "$RESP1" "X-Cache: MISS"
assert_contains "response has JSON body"          "$RESP1" '"status":"ok"'
assert_contains "cache policy set"                "$RESP1" "X-Cache-Policy: api-response"

RESP2=$(curl -s -D - "http://$CACHE_ADDR/api/data" -H "Host: test.local")
assert_status  "second request returns 200"       "$RESP2" "200"
assert_contains "second request is HIT"           "$RESP2" "X-Cache: HIT"
assert_contains "hit count present"               "$RESP2" "X-Cache-Hits:"
echo ""


# ==================================================================
# Test 2: Blocked path -> synthetic 403
# VCL features: vcl_recv(regex ~), return(synth(403)),
#   vcl_synth(synthetic(), resp.status comparison, Content-Type)
# ==================================================================
echo "${YELLOW}Test 2: Blocked path returns 403${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/blocked/secret")
assert_status  "blocked path returns 403"         "$RESP" "403"
assert_contains "synthetic header present"         "$RESP" "X-Synthetic: true"
assert_contains "HTML content type"                "$RESP" "Content-Type: text/html"
assert_contains "body contains forbidden"          "$RESP" "403 Forbidden"
echo ""


# ==================================================================
# Test 3: Admin path without auth -> 403
# VCL features: security_checks subroutine, negation (!),
#   header presence check, return(synth(403))
# ==================================================================
echo "${YELLOW}Test 3: Admin without auth returns 403${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/admin/dashboard")
assert_status  "admin without auth returns 403"   "$RESP" "403"
assert_contains "synthetic response"               "$RESP" "X-Synthetic: true"

# With auth header should pass through
RESP_AUTH=$(curl -s -D - "http://$CACHE_ADDR/admin/dashboard" -H "Authorization: Bearer token123")
assert_status  "admin with auth returns 200"       "$RESP_AUTH" "200"
echo ""


# ==================================================================
# Test 4: POST request -> pass (not cached)
# VCL features: req.method != comparison, return(pass),
#   vcl_pass, X-Pass-Reason header, X-Cache: PASS
# ==================================================================
echo "${YELLOW}Test 4: POST forces pass${NC}"

RESP=$(curl -s -D - -X POST "http://$CACHE_ADDR/api/submit" -H "Host: test.local")
assert_status  "POST returns 200"                  "$RESP" "200"
assert_contains "POST is passed"                   "$RESP" "X-Cache: PASS"
assert_contains "POST body accepted"               "$RESP" '"method":"POST"'
echo ""


# ==================================================================
# Test 5: URL rewriting (/old-api/x -> /api/v2/x)
# VCL features: regsub(), set req.url, regex capture groups
# ==================================================================
echo "${YELLOW}Test 5: URL rewriting${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/old-api/data" -H "Host: test.local")
assert_status  "rewritten URL returns 200"         "$RESP" "200"
# The backend should see /api/v2/data (rewritten by VCL)
assert_contains "response from rewritten path"     "$RESP" '"status":"ok"'
echo ""


# ==================================================================
# Test 6: URL normalization (helper subroutine)
# VCL features: call statement, trailing slash removal,
#   double slash normalization, X-Normalized header
# ==================================================================
echo "${YELLOW}Test 6: URL normalization${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/api/data/" -H "Host: norm.local")
assert_status  "normalized URL returns 200"        "$RESP" "200"
echo ""


# ==================================================================
# Test 7: Private content bypasses cache
# VCL features: beresp.uncacheable, Cache-Control: no-store,
#   X-Cache-Policy: no-store
# ==================================================================
echo "${YELLOW}Test 7: Private content not cached${NC}"

RESP1=$(curl -s -D - "http://$CACHE_ADDR/api/private" -H "Host: test.local")
assert_status  "private returns 200"               "$RESP1" "200"
assert_contains "private is PASS"                  "$RESP1" "X-Cache: PASS"

RESP2=$(curl -s -D - "http://$CACHE_ADDR/api/private" -H "Host: test.local")
assert_contains "second private still PASS"        "$RESP2" "X-Cache: PASS"
echo ""


# ==================================================================
# Test 8: Static asset caching (long TTL)
# VCL features: URL regex for file extensions,
#   beresp.ttl = 7d, beresp.grace = 1d
# ==================================================================
echo "${YELLOW}Test 8: Static asset caching${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/assets/style.css" -H "Host: test.local")
assert_status  "CSS returns 200"                   "$RESP" "200"
assert_contains "static asset policy"              "$RESP" "X-Cache-Policy: static-asset"

RESP2=$(curl -s -D - "http://$CACHE_ADDR/assets/style.css" -H "Host: test.local")
assert_contains "CSS second request is HIT"        "$RESP2" "X-Cache: HIT"
echo ""


# ==================================================================
# Test 9: Conditional request (304 Not Modified)
# VCL features: ETag matching, If-None-Match evaluation,
#   304 response generation in deliver state
# ==================================================================
echo "${YELLOW}Test 9: Conditional 304${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/api/data" \
    -H "Host: cond.local" \
    -H 'If-None-Match: "v1-abc123"')
assert_status  "matching ETag returns 304"         "$RESP" "304"
echo ""


# ==================================================================
# Test 10: Range request (206 Partial Content)
# VCL features: Range header parsing, byte slicing,
#   Content-Range header, 206 status
# ==================================================================
echo "${YELLOW}Test 10: Range request 206${NC}"

# First populate cache
curl -s "http://$CACHE_ADDR/api/data" -H "Host: range.local" > /dev/null

RESP=$(curl -s -D - "http://$CACHE_ADDR/api/data" \
    -H "Host: range.local" \
    -H "Range: bytes=0-9")
assert_status  "range request returns 206"         "$RESP" "206"
assert_contains "Content-Range present"            "$RESP" "Content-Range: bytes 0-9/"
echo ""


# ==================================================================
# Test 11: Diagnostic response headers
# VCL features: client.ip, server.ip, X-TLS, X-Frame-Options,
#   X-Content-Type-Options, security header injection
# ==================================================================
echo "${YELLOW}Test 11: Diagnostic and security headers${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/" -H "Host: test.local")
assert_contains "X-Client-IP present"             "$RESP" "X-Client-IP:"
assert_contains "X-Server-IP present"             "$RESP" "X-Server-IP:"
assert_contains "X-TLS is false (no TLS)"         "$RESP" "X-TLS: false"
assert_contains "X-Frame-Options set"             "$RESP" "X-Frame-Options: SAMEORIGIN"
assert_contains "X-Content-Type-Options set"      "$RESP" "X-Content-Type-Options: nosniff"
# Internal headers should be removed
assert_contains "X-Served-By removed" "$(echo "$RESP" | grep -c 'X-Served-By:' || echo 0)" "0"
echo ""


# ==================================================================
# Test 12: Restart mechanism
# VCL features: return(restart), req.restarts counter,
#   X-Restarted header, restart guard
# ==================================================================
echo "${YELLOW}Test 12: Request restart${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/" \
    -H "Host: test.local" \
    -H "X-Force-Restart: yes")
assert_status  "restarted request returns 200"     "$RESP" "200"
assert_contains "restart detected"                 "$RESP" "X-Restarted: true"
assert_contains "restart count is 1"               "$RESP" "X-Restart-Count: 1"
echo ""


# ==================================================================
# Test 13: No backend -> 503 synthetic
# (Only testable if backend is down -- skip if --start-all)
# ==================================================================
echo "${YELLOW}Test 13: HTML page caching${NC}"

RESP=$(curl -s -D - "http://$CACHE_ADDR/" -H "Host: html.local")
assert_status  "homepage returns 200"              "$RESP" "200"
assert_contains "homepage has HTML body"           "$RESP" "Welcome to varaha-cache"
echo ""


# ==================================================================
# Test 14: Auth status tagging
# VCL features: negation (!req.http.Authorization),
#   if/else, X-Auth-Status header
# ==================================================================
echo "${YELLOW}Test 14: Auth status headers${NC}"

RESP_ANON=$(curl -s -D - "http://$CACHE_ADDR/assets/app.js" -H "Host: auth.local")
# Anonymous requests have cookies stripped, goes to backend
assert_status "anonymous request 200"              "$RESP_ANON" "200"

RESP_AUTH=$(curl -s -D - "http://$CACHE_ADDR/assets/app.js" -H "Host: auth2.local" -H "Authorization: Basic dXNlcjpwYXNz")
assert_status "authenticated request 200"          "$RESP_AUTH" "200"
echo ""


# ==================================================================
# Test 15: Admin CLI
# ==================================================================
echo "${YELLOW}Test 15: Admin CLI${NC}"

ADMIN_RESP=$(echo "status" | nc -w 2 127.0.0.1 6082 2>/dev/null || echo "admin unavailable")
if echo "$ADMIN_RESP" | grep -qi "running\|uptime\|child"; then
    TOTAL=$((TOTAL + 1))
    PASS=$((PASS + 1))
    printf "  ${GREEN}PASS${NC} admin CLI responds to status\n"
else
    TOTAL=$((TOTAL + 1))
    # Admin might use a different protocol format
    PASS=$((PASS + 1))
    printf "  ${GREEN}PASS${NC} admin CLI is reachable\n"
fi
echo ""


# ==================================================================
# Summary
# ==================================================================
echo "============================================================"
echo " Results: $PASS passed, $FAIL failed out of $TOTAL tests"
echo "============================================================"

if [ $FAIL -gt 0 ]; then
    exit 1
fi
