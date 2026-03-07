vcl 4.0;

backend default {
    .host = "172.28.0.10";
    .port = "8132";
}

sub vcl_recv {
    return(hash);
}

sub vcl_backend_response {
    set beresp.ttl = 120s;
}
