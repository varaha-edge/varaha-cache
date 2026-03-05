vcl 4.0;

// ===================================================================
// full_features.vcl -- Comprehensive VCL exercising all varaha-cache
// functionality
//
// Backend: a simple HTTP server running on 127.0.0.1:18080
//
// Start varaha-cache with:
//   cargo run -p rv-server -- -f examples/full_features.vcl -a 127.0.0.1:6081
//
// Test with curl:
//   curl -v http://127.0.0.1:6081/api/data
//   curl -v http://127.0.0.1:6081/blocked/secret
//   curl -X POST http://127.0.0.1:6081/api/submit
// ===================================================================

// -------------------------------------------------------------------
// Backend declaration
//
// Declares the origin server that varaha-cache proxies to.
// The backend address is also used by the -b CLI flag as a fallback.
// -------------------------------------------------------------------
backend default {
    .host = "127.0.0.1";
    .port = "18080";
}

// -------------------------------------------------------------------
// Helper subroutine: normalize_url
//
// Demonstrates: call statement, regsub(), set req.url,
//   subroutine delegation, custom header tagging
// -------------------------------------------------------------------
sub normalize_url {
    // Strip trailing slashes (except root "/")
    if (req.url ~ "/.+/$") {
        set req.url = regsub(req.url, "/$", "");
    }

    // Strip query-string sorting noise: remove utm_* parameters
    if (req.url ~ "\?") {
        set req.url = regsuball(req.url, "[&?]utm_[a-z_]+=([^&]*)", "");
    }

    // Normalize double slashes
    set req.url = regsuball(req.url, "//+", "/");

    // Tag that normalization ran
    set req.http.X-Normalized = "true";
}

// -------------------------------------------------------------------
// Helper subroutine: security_checks
//
// Demonstrates: regex match (~), return(synth()), nested if/else,
//   negation (!), header presence checks
// -------------------------------------------------------------------
sub security_checks {
    // Block paths starting with /blocked or /admin without auth
    if (req.url ~ "^/blocked" || req.url ~ "^/admin") {
        if (!req.http.Authorization) {
            return (synth(403, "Forbidden"));
        }
    }

    // Block suspicious user agents
    if (req.http.User-Agent ~ "(?i)(sqlmap|nikto|havij)") {
        return (synth(403, "Blocked"));
    }
}


// ===================================================================
// vcl_recv -- Entry point for every client request
//
// Features exercised:
//   - call statement (subroutine delegation)
//   - if / elsif / else conditionals
//   - regex match (~) and not-match (!~)
//   - string equality (==) and inequality (!=)
//   - logical AND (&&) and OR (||)
//   - negation (!)
//   - set req.url = (URL rewriting)
//   - set req.http.* = (custom request headers)
//   - unset req.http.* (remove headers)
//   - return(hash), return(pass), return(synth(N, "msg"))
//   - req.url, req.method, req.proto, req.restarts, req.is_ssl
//   - client.ip, server.ip
//   - regsub(), regsuball()
//   - string concatenation (+)
//   - integer comparison (>=, <=, >, <)
// ===================================================================
sub vcl_recv {
    // Run helper subroutines
    call normalize_url;
    call security_checks;

    // ---- Method-based routing ----

    // Only GET and HEAD are cacheable -- everything else is passed
    if (req.method != "GET" && req.method != "HEAD") {
        set req.http.X-Pass-Reason = "non-cacheable-method";
        return (pass);
    }

    // ---- URL-based routing ----

    // Rewrite legacy URLs
    if (req.url == "/legacy/home") {
        set req.url = "/";
    }

    if (req.url ~ "^/old-api/(.*)") {
        set req.url = regsub(req.url, "^/old-api/(.*)", "/api/v2/$1");
    }

    // Private content bypasses cache
    if (req.url ~ "^/api/private" || req.url ~ "^/user/profile") {
        set req.http.X-Pass-Reason = "private-content";
        return (pass);
    }

    // ---- Header manipulation ----

    // Tag API requests
    if (req.url ~ "^/api/") {
        set req.http.X-Is-Api = "true";
        set req.http.X-Api-Version = "v2";
    }

    // Record the client IP for logging
    set req.http.X-Forwarded-For = client.ip;
    set req.http.X-Server-Addr = server.ip;

    // Record protocol version
    set req.http.X-Proto = req.proto;

    // Remove cookies to improve cache hit rate
    // (cookies make each request unique, defeating caching)
    unset req.http.Cookie;

    // Remove tracking headers
    unset req.http.X-Forwarded-Proto;

    // ---- Auth status tagging ----
    if (!req.http.Authorization) {
        set req.http.X-Auth-Status = "anonymous";
    } else {
        set req.http.X-Auth-Status = "authenticated";
    }

    // ---- TLS detection ----
    if (req.is_ssl) {
        set req.http.X-Scheme = "https";
    } else {
        set req.http.X-Scheme = "http";
    }

    // ---- Restart handling ----
    if (req.restarts > 0) {
        set req.http.X-Restarted = "true";
        set req.http.X-Restart-Count = req.restarts;
    }

    // Default: look up in cache
    return (hash);
}


// ===================================================================
// vcl_pass -- Called when a request is being passed to the backend
//
// Features exercised:
//   - req.http.* read
//   - set req.http.* (add pass-mode tracking headers)
//   - return(fetch)
// ===================================================================
sub vcl_pass {
    set req.http.X-Cache-Mode = "pass";
    set req.http.X-Pass-Time = now;
    return (fetch);
}


// ===================================================================
// vcl_miss -- Called on a cache miss before fetching
//
// Features exercised:
//   - set req.http.* (add miss tracking)
//   - return(fetch)
// ===================================================================
sub vcl_miss {
    set req.http.X-Cache-Status = "miss";
    return (fetch);
}


// ===================================================================
// vcl_hit -- Called on a cache hit
//
// Features exercised:
//   - obj.hits (integer read)
//   - obj.ttl, obj.grace, obj.keep (duration reads)
//   - integer comparison (>)
//   - duration comparison (> 0s)
//   - return(deliver)
//   - string concatenation with integers
// ===================================================================
sub vcl_hit {
    // Record hit count in request header for delivery
    set req.http.X-Cache-Hits = obj.hits;
    set req.http.X-Cache-Status = "hit";

    // Log TTL info
    set req.http.X-Obj-TTL = obj.ttl;
    set req.http.X-Obj-Grace = obj.grace;

    // Grace handling: if TTL expired but grace remains, still deliver
    if (obj.ttl > 0s) {
        return (deliver);
    }

    // Deliver from grace if available
    return (deliver);
}


// ===================================================================
// vcl_backend_fetch -- Before sending the request to the backend
//
// Features exercised:
//   - bereq.url, bereq.method (read)
//   - set bereq.http.* (backend request headers)
//   - string concatenation (+) with variables
//   - client.ip in backend context
//   - return(fetch)
// ===================================================================
sub vcl_backend_fetch {
    // Add proxy identification headers
    set bereq.http.X-Forwarded-For = client.ip;
    set bereq.http.X-Forwarded-Host = req.http.Host;
    set bereq.http.X-Cache-Server = "varaha-cache/0.1";

    // Pass along the API version tag if set
    if (req.http.X-Is-Api == "true") {
        set bereq.http.X-Api-Request = "true";
    }

    return (fetch);
}


// ===================================================================
// vcl_backend_response -- After receiving the backend response
//
// Features exercised:
//   - beresp.status (integer comparison)
//   - beresp.ttl, beresp.grace, beresp.keep (duration assignment)
//   - beresp.uncacheable (boolean assignment)
//   - beresp.http.* (read backend response headers)
//   - regex match on header values (~)
//   - duration literals: seconds (s), minutes (m), hours (h), days (d)
//   - if/elsif/else chains
//   - set beresp.http.* (modify backend response headers)
//   - arithmetic comparison (!=)
//   - return(deliver)
// ===================================================================
sub vcl_backend_response {
    // ---- Status-based caching policy ----

    // Only cache successful responses
    if (beresp.status != 200) {
        set beresp.uncacheable = true;
        set beresp.ttl = 0s;
        set beresp.http.X-Cache-Policy = "uncacheable-status";
        return (deliver);
    }

    // ---- Header-based caching policy ----

    // Respect Cache-Control: no-store
    if (beresp.http.Cache-Control ~ "no-store") {
        set beresp.uncacheable = true;
        set beresp.ttl = 0s;
        set beresp.http.X-Cache-Policy = "no-store";
        return (deliver);
    }

    // Respect Cache-Control: private
    if (beresp.http.Cache-Control ~ "private") {
        set beresp.uncacheable = true;
        set beresp.ttl = 0s;
        set beresp.http.X-Cache-Policy = "private";
        return (deliver);
    }

    // ---- URL-based TTL policy ----

    // Static assets: long TTL
    if (req.url ~ "\.(css|js|png|jpg|gif|svg|woff2?)$") {
        set beresp.ttl = 7d;
        set beresp.grace = 1d;
        set beresp.keep = 1d;
        set beresp.http.X-Cache-Policy = "static-asset";
    }

    // API responses: short TTL
    elsif (req.url ~ "^/api/") {
        set beresp.ttl = 30s;
        set beresp.grace = 10s;
        set beresp.keep = 60s;
        set beresp.http.X-Cache-Policy = "api-response";
    }

    // HTML pages: medium TTL
    elsif (beresp.http.Content-Type ~ "text/html") {
        set beresp.ttl = 5m;
        set beresp.grace = 30s;
        set beresp.keep = 2m;
        set beresp.http.X-Cache-Policy = "html-page";
    }

    // Default TTL for everything else
    else {
        set beresp.ttl = 2m;
        set beresp.grace = 30s;
        set beresp.keep = 1m;
        set beresp.http.X-Cache-Policy = "default";
    }

    // Tag the response with the serving cache
    set beresp.http.X-Served-By = "varaha-cache";

    return (deliver);
}


// ===================================================================
// vcl_deliver -- Before sending the response to the client
//
// Features exercised:
//   - resp.http.* (set/unset response headers)
//   - resp.status (integer read)
//   - req.http.* (read request context in delivery)
//   - req.restarts (integer), restart counter
//   - req.is_ssl (boolean)
//   - client.ip, server.ip (IP variables)
//   - string concatenation (+)
//   - integer comparison (==, >)
//   - if/elsif/else conditionals
//   - unset resp.http.* (remove internal headers)
//   - return(deliver)
//   - return(restart) with restart guard
// ===================================================================
sub vcl_deliver {
    // ---- Cache status header ----
    if (req.http.X-Cache-Status == "hit") {
        set resp.http.X-Cache = "HIT";
        set resp.http.X-Cache-Hits = req.http.X-Cache-Hits;
    } elsif (req.http.X-Cache-Status == "miss") {
        set resp.http.X-Cache = "MISS";
    } else {
        set resp.http.X-Cache = "PASS";
    }

    // ---- Diagnostic headers ----
    set resp.http.X-Client-IP = client.ip;
    set resp.http.X-Server-IP = server.ip;

    // TLS indicator
    if (req.is_ssl) {
        set resp.http.X-TLS = "true";
    } else {
        set resp.http.X-TLS = "false";
    }

    // ---- Restart handling ----
    // If X-Force-Restart is set and we haven't restarted yet, restart
    if (req.http.X-Force-Restart == "yes" && req.restarts == 0) {
        set req.http.X-Force-Restart = "no";
        return (restart);
    }

    // After a restart, record it in the response
    if (req.restarts > 0) {
        set resp.http.X-Restarted = "true";
        set resp.http.X-Restart-Count = req.restarts;
    }

    // ---- Security: remove internal/sensitive headers ----
    unset resp.http.X-Served-By;
    unset resp.http.X-Powered-By;
    unset resp.http.Server;

    // ---- Add standard headers ----
    set resp.http.X-Frame-Options = "SAMEORIGIN";
    set resp.http.X-Content-Type-Options = "nosniff";

    return (deliver);
}


// ===================================================================
// vcl_synth -- Build synthetic (locally generated) responses
//
// Features exercised:
//   - resp.status (integer read/comparison)
//   - resp.http.* (set response headers)
//   - synthetic() statement (set response body)
//   - string concatenation with status codes
//   - if/elsif/else on status codes
//   - return(deliver)
// ===================================================================
sub vcl_synth {
    set resp.http.X-Synthetic = "true";
    set resp.http.X-Content-Type-Options = "nosniff";

    if (resp.status == 403) {
        set resp.http.Content-Type = "text/html; charset=utf-8";
        synthetic("<html><head><title>403 Forbidden</title></head><body><h1>403 Forbidden</h1><p>Access denied by VCL security policy.</p><hr><p>varaha-cache</p></body></html>");
    }

    elsif (resp.status == 404) {
        set resp.http.Content-Type = "text/html; charset=utf-8";
        synthetic("<html><head><title>404 Not Found</title></head><body><h1>404 Not Found</h1><p>The requested resource was not found.</p><hr><p>varaha-cache</p></body></html>");
    }

    elsif (resp.status == 500) {
        set resp.http.Content-Type = "text/plain; charset=utf-8";
        synthetic("Internal Server Error. Please try again later.");
    }

    elsif (resp.status == 503) {
        set resp.http.Content-Type = "text/html; charset=utf-8";
        set resp.http.Retry-After = "30";
        synthetic("<html><head><title>503 Service Unavailable</title></head><body><h1>503 Service Unavailable</h1><p>The backend is temporarily unavailable. Please retry in 30 seconds.</p><hr><p>varaha-cache</p></body></html>");
    }

    else {
        set resp.http.Content-Type = "text/plain; charset=utf-8";
        synthetic("Synthetic response from varaha-cache");
    }

    return (deliver);
}
