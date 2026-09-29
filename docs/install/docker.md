# Running OXIM with Docker

The image `ghcr.io/oxim-health/oxim` is built from [`deploy/docker/Dockerfile`](../../deploy/docker/Dockerfile) for `linux/amd64` and `linux/arm64`. It runs `oxim run -c /etc/oxim/oxim.yaml` as the unprivileged user `oxim` (uid and gid 10001).

| Path | Contents |
|---|---|
| `/etc/oxim/oxim.yaml` | configuration from `oxim init`, with `data_dir: /var/lib/oxim` |
| `/etc/oxim/channels` | channel files; contains an inactive example |
| `/etc/oxim/tables` | code and routing tables |
| `/var/lib/oxim` | volume: message database and the working directory |

## Run

```sh
docker volume create oxim-data
docker run -d --name oxim --restart unless-stopped \
  -v oxim-data:/var/lib/oxim \
  -v "$PWD/channels:/etc/oxim/channels:ro" \
  -v "$PWD/tables:/etc/oxim/tables:ro" \
  -p 2575:2575 \
  ghcr.io/oxim-health/oxim:<version>
docker logs -f oxim
```

Publish the ports your channels listen on. Channels must listen on `0.0.0.0` (or the container's address) to be reachable from outside the container. Channel files are picked up while the container runs.

The image's web server listens on port 8080 inside the container. Publish it only where it is needed, for example `-p 127.0.0.1:8080:8080`, and create the first administrator:

```sh
docker exec -it oxim oxim -c /etc/oxim/oxim.yaml users create-admin --username admin
docker restart oxim
```

Check the channels:

```sh
docker exec oxim oxim -c /etc/oxim/oxim.yaml validate
```

To use your own `oxim.yaml`, mount it at `/etc/oxim/oxim.yaml`; keep `data_dir` on the volume. The container stops gracefully on `docker stop`; allow in-flight deliveries to finish with `--stop-timeout 75`.

Serial analyzers need the device and the `dialout` group:

```sh
docker run ... --device /dev/ttyUSB0 --group-add dialout ghcr.io/oxim-health/oxim:<version>
```

## Demo

[`deploy/docker/compose.yaml`](../../deploy/docker/compose.yaml) starts OXIM with a simulated ASTM analyzer and a simulated LIS. The analyzer sends synthetic results every 30 seconds; OXIM translates the test codes and delivers HL7 v2 ORU^R01 messages to the LIS, which prints them:

```sh
docker compose -f deploy/docker/compose.yaml up --build
docker compose -f deploy/docker/compose.yaml logs -f lis
```

All data in the demo is synthetic.

## Build the image

From the repository root:

```sh
docker build -f deploy/docker/Dockerfile -t oxim .
docker build -f deploy/docker/Dockerfile --target sim -t oxim-sim .
```

`--build-arg RUST_VERSION=<version>` selects the Rust toolchain (1.89 or later).
