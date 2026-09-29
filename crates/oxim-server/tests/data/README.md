# Test data

`localhost.crt` and `localhost.key` are a self-signed certificate and key
for `localhost` and `127.0.0.1`, generated for the HTTPS test only:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -keyout localhost.key -out localhost.crt -days 36500 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:FALSE"
```

They protect nothing; never use them outside tests.
