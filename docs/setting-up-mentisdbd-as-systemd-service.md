# Setting Up mentisdbd as a Linux Systemd Service

**April 12, 2026**

This guide shows you how to run `mentisdbd` as a proper background service using systemd — the standard on almost all modern Linux distributions (Ubuntu, Debian, Fedora, Arch, etc.).

This is the recommended way to run MentisDB persistently on a server or always-on workstation.

---

## 1. Build and Install the Binary

```bash
cd ~/workspace/mentisdb
make build-mentisdbd        # or cargo build --release
sudo cp target/release/mentisdbd /usr/local/bin/
sudo chmod +x /usr/local/bin/mentisdbd
```

---

## 2. Create the Environment File

```bash
sudo mkdir -p /etc/mentisdb
sudo tee /etc/mentisdb/mentisdbd.env > /dev/null <<EOF
MENTISDB_DIR=/var/lib/mentisdb
MENTISDB_DEFAULT_CHAIN_KEY=borganism-brain
MENTISDB_BIND_HOST=127.0.0.1
MENTISDB_MCP_PORT=9471
MENTISDB_REST_PORT=9472
MENTISDB_DASHBOARD_PORT=9475
MENTISDB_VERBOSE=true
MENTISDB_AUTO_FLUSH=true
RUST_LOG=info
EOF

sudo chmod 640 /etc/mentisdb/mentisdbd.env
```

---

## 3. Create the Systemd Service

Create the file `/etc/systemd/system/mentisdbd.service`:

```ini
[Unit]
Description=MentisDB Daemon - Durable Semantic Memory Engine
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=mentisdb
Group=mentisdb
EnvironmentFile=/etc/mentisdb/mentisdbd.env
ExecStart=/usr/local/bin/mentisdbd
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
SyslogIdentifier=mentisdbd
WorkingDirectory=/var/lib/mentisdb

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/mentisdb

[Install]
WantedBy=multi-user.target
```

---

## 4. Create Dedicated User and Directories

```bash
sudo useradd -r -s /sbin/nologin -m -d /var/lib/mentisdb mentisdb
sudo mkdir -p /var/lib/mentisdb
sudo chown -R mentisdb:mentisdb /var/lib/mentisdb
sudo chown root:mentisdb /etc/mentisdb/mentisdbd.env
```

---

## 5. Enable and Start the Service

```bash
sudo systemctl daemon-reload
sudo systemctl enable mentisdbd
sudo systemctl start mentisdbd
sudo systemctl status mentisdbd
```

---

## Useful Commands

```bash
# View live logs
sudo journalctl -u mentisdbd -f

# View last 100 lines
sudo journalctl -u mentisdbd -n 100

# Restart service
sudo systemctl restart mentisdbd

# Stop service
sudo systemctl stop mentisdbd
```

---

## Alternative: Run as Your Current User

If you prefer to run it under your own user instead of creating a `mentisdb` system user, change the service file to:

```ini
User=user
Group=user
```

(Replace `user` with your actual username.)

Then remove the `mentisdb` user creation step.

---

## 6. Verify the Daemon is Working

Don't just trust `systemctl status`. Actually test the daemon:

```bash
# Test REST API health endpoint
curl -k https://localhost:9474/health 2>/dev/null || echo "REST server not responding"

# Check if daemon is listening on MCP port
ss -tlnp | grep 9471

# Test with mentisdbd CLI (add a test thought)
mentisdbd add "Test from systemd" --type Summary --tag systemd-test

# List recent thoughts to verify
curl -k "https://localhost:9474/chains/default/thoughts?limit=1" 2>/dev/null | head -c 200
```

---

## 7. Configure Your MCP Clients

Once the daemon is running as a service, configure your AI clients to connect to it:

| Client | Setup Command |
|--------|---------------|
| Claude Code | `mentisdbd setup claude-code` |
| Codex | `mentisdbd setup codex` |
| Claude Desktop | `mentisdbd setup claude-desktop` |
| OpenCode | `mentisdbd setup opencode` |
| Gemini | `mentisdbd setup gemini` |
| VS Code Copilot | `mentisdbd setup vscode-copilot` |
| All clients | `mentisdbd setup all` |

Or run the interactive wizard to auto-detect:
```bash
mentisdbd wizard
```

---

## Log Rotation (Recommended)

The service uses journald. Add log rotation to prevent disk fill:

```bash
sudo tee /etc/systemd/journald.conf.d/mentisdb.conf > /dev/null <<EOF
[Journal]
SystemMaxUse=500M
SystemMaxFileSize=50M
MaxFileSec=1week
EOF

sudo systemctl restart systemd-journald
```

---

The daemon will now start automatically on boot and restart if it ever crashes.

For more configuration options, see the [main README](../README.md#daemon-configuration).

