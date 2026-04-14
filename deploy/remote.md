# Remote Deployment — vox bridge to non-local omegon daemon

The omegon daemon binds to `127.0.0.1` by design. Plain HTTP over a
network is a degraded transport. For remote vox→daemon connectivity,
use one of these secure transport options.

## Option 1: SSH tunnel (simplest, zero infra)

The vox bridge runs on the same host as SSH and tunnels port 7842 to
the remote daemon. Works everywhere, zero additional dependencies.

```bash
# On the vox host — tunnel to the omegon host
ssh -N -L 7842:127.0.0.1:7842 omegon-host &

# Then run vox normally — it thinks it's talking to localhost
VOX_DISCORD_BOT_TOKEN="..." vox --bridge --daemon-url http://127.0.0.1:7842
```

For persistent tunnels, use autossh:
```bash
autossh -M 0 -f -N -L 7842:127.0.0.1:7842 omegon-host
```

## Option 2: Tailscale (recommended for lab/fleet)

Tailscale gives you mutual-auth HTTPS with zero cert management.
Both hosts must be on the same tailnet.

```bash
# On the omegon host — expose daemon over tailnet
tailscale serve --bg 7842

# On the vox host — use the tailnet hostname
VOX_DISCORD_BOT_TOKEN="..." vox --bridge \
    --daemon-url https://omegon-host.tail12345.ts.net:7842
```

`tailscale serve` wraps the localhost listener in tailnet-only HTTPS.
No public exposure, no cert rotation, mutual WireGuard authentication.

## Option 3: Styrene tunnel (mesh-native, airgapped)

For deployments where neither SSH nor Tailscale is available (airgapped,
mesh-only networks), use styrene-tunnel to create a PQC-encrypted
tunnel over Reticulum.

```bash
# On the omegon host
styrene-tunnel serve --local 127.0.0.1:7842 --announce omegon-daemon

# On the vox host
styrene-tunnel connect --destination omegon-daemon --local-port 7842

# Then vox connects to localhost as usual
VOX_DISCORD_BOT_TOKEN="..." vox --bridge --daemon-url http://127.0.0.1:7842
```

This works over any Reticulum transport (LoRa, packet radio, serial,
TCP, UDP, I2P) with no internet dependency.

## Option 4: Reverse proxy with TLS (traditional)

Put nginx or caddy in front of the daemon with a real TLS certificate.

```nginx
# /etc/nginx/sites-enabled/omegon
server {
    listen 443 ssl;
    server_name omegon.lab.example.com;
    ssl_certificate     /etc/letsencrypt/live/omegon.lab.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/omegon.lab.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:7842;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
    }
}
```

```bash
VOX_DISCORD_BOT_TOKEN="..." vox --bridge \
    --daemon-url https://omegon.lab.example.com
```

## Authentication

All options above secure the transport. The daemon event API also
requires a bearer token:

```bash
# Get the token from omegon's startup output or /api/startup
curl -s http://127.0.0.1:7842/api/startup | jq -r .token

# Pass it to vox
vox --bridge --daemon-url https://omegon-host --daemon-token "the-token"
```

## Deployment topology

```
┌─ Host A (lab server) ──────────────┐
│  omegon serve (127.0.0.1:7842)     │
│  └─ session router, agent turns    │
└────────────────────────────────────┘
         ▲
         │ SSH tunnel / Tailscale / styrene-tunnel / TLS proxy
         │
┌─ Host B (edge / cloud / RPi) ─────┐
│  vox --bridge                      │
│  └─ Discord gateway connected      │
│  └─ polls connectors, pushes       │
│     events to Host A               │
└────────────────────────────────────┘
```

vox is stateless except for connector sessions (Discord gateway, etc.).
It can run anywhere with internet access for the Discord/Slack APIs.
The omegon daemon runs where the code and tools live — typically the
lab server with git repos, build tools, and local inference.
