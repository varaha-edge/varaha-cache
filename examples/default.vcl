vcl 4.0;

backend default {
    .host = "127.0.0.1";
    .port = "18080";
}

sub vcl_recv {
    return(hash);
}

sub vcl_backend_response {
    set beresp.ttl = 120s;
}
