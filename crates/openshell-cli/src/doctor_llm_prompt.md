<!-- Derived from .agents/skills/debug-openshell-cluster/SKILL.md -->
<!-- Keep in sync when updating cluster debug procedures -->

# Debug OpenShell Gateway (macOS / Apple Container)

You are diagnosing an OpenShell gateway running on macOS. The gateway runs as a **native macOS process** (not inside a container). Sandboxes are **Apple Container VMs** managed via the `container` CLI (Apple's Virtualization.framework). There is no Docker, no k3s, no Kubernetes. Run diagnostics automatically through the steps below in order. Stop and report findings as soon as a root cause is identified.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ macOS host                                                   │
│                                                              │
│  openshell-server (native process, PID file managed)         │
│    ├── gRPC API on port 8080 (configurable via --port)       │
│    ├── mTLS authentication (PKI in ~/.openshell/)            │
│    ├── SQLite database for state                             │
│    └── sandbox backend: apple-container                      │
│           │                                                  │
│           ├── container create / start / stop / delete       │
│           │   (calls `container` CLI as subprocesses)        │
│           │                                                  │
│           └── Apple Container VMs (vmnet networking)         │
│               ├── openshell-sandbox-<name> (192.168.65.x)    │
│               ├── SSH on port 2222 (bootstrapped via exec)   │
│               └── Linux kernel per VM                        │
└──────────────────────────────────────────────────────────────┘
```

## File System Layout

```
~/.openshell/gateways/<name>/
  gateway.pid              # PID of the running openshell-server process
  pki/
    ca.crt                 # Certificate authority
    tls.crt                # Server TLS certificate (also used for client mTLS)
    tls.key                # Server TLS private key
  data/
    openshell.db           # SQLite database (sandbox state, config)
  logs/
    server.log             # Combined stdout/stderr from openshell-server
```

The default gateway name is `openshell`, so the default path is `~/.openshell/gateways/openshell/`.

## Tools Available

Use these commands for diagnostics:

```bash
# Quick connectivity check (run this first)
openshell status

# Fetch gateway process logs
openshell doctor logs --lines 100
openshell doctor logs --tail          # stream live

# Check if the gateway process is alive
cat ~/.openshell/gateways/<name>/gateway.pid
ps aux | grep openshell-server

# Apple Container VM status
container list --all --format json
container system status

# Check a specific sandbox VM
container exec <container-name> sh -c 'ss -tlnp | grep 2222'
```

## Bootstrap Sequence

`openshell gateway start` deploys the gateway as a native macOS process. The stages, in order, are:

1. **Detect Apple Container**: Run `container system status` to verify Apple Container is installed and running. If not running, abort with a message to run `container system start`.
2. **Locate server binary**: Search for `openshell-server` in this order:
   - `OPENSHELL_SERVER_BIN` environment variable
   - Same directory as the current `openshell` CLI binary
   - `PATH` lookup via `which openshell-server`
3. **Create directory structure**: Create `~/.openshell/gateways/<name>/` with `data/`, `pki/`, and `logs/` subdirectories.
4. **Generate PKI**: Create mTLS certificates (CA, server cert, server key) using `rcgen`. Store in `pki/` directory. Certificates effectively never expire.
5. **Generate SSH handshake secret**: Random 32-byte hex secret for sandbox SSH HMAC authentication.
6. **Start gateway process**: Launch `openshell-server` as a background process with:
   - `--port <port>` (default 8080)
   - `--sandbox-backend apple-container`
   - `--db-url sqlite://<data_dir>/openshell.db`
   - `--ssh-handshake-secret <secret>`
   - `--sandbox-image <image>` (default `ghcr.io/nvidia/openshell-community/sandboxes/base:latest`)
   - stdout/stderr redirected to `logs/server.log`
7. **Write PID file**: Write the child process PID to `gateway.pid` for lifecycle management.
8. **TCP health probe**: Attempt `TcpStream::connect("127.0.0.1:<port>")` up to 30 times at 1-second intervals. If the process dies during this wait, read the last 10 lines of `server.log` and report the error.

The host port is configurable via `--port` on `openshell gateway start` (default 8080) and is stored in gateway metadata.

## Sandbox Creation Sequence

When a sandbox is created, the gateway process manages the full lifecycle via the `container` CLI:

1. **Ensure image**: Check if the sandbox image exists locally (`container image list --format json`). If not, pull it (`container image pull <image>`).
2. **Create container**: Run `container create --name openshell-sandbox-<name> -d --entrypoint /bin/sh -e KEY=VALUE ... <image> -c "exec sleep infinity"`. The `sleep infinity` entrypoint keeps the VM alive (the base image's default `/bin/bash` exits without a TTY). Environment variables include `OPENSHELL_GRPC_ENDPOINT`, `OPENSHELL_SANDBOX_ID`, SSH handshake secrets, and user-specified env vars.
3. **Start container**: Run `container start openshell-sandbox-<name>`.
4. **Wait for network**: Sleep 2 seconds for the VM to boot and get a vmnet interface.
5. **Bootstrap SSH**: Via `container exec --uid 0`, install and configure openssh-server:
   - `apt-get install openssh-server`
   - Configure sshd on port 2222
   - Enable empty password authentication
   - Create a user matching the host `$USER` (uid 1000) for seamless SSH auth
   - Clear passwords for the `sandbox` user and the host-matching user
   - Start sshd: `/usr/sbin/sshd -p 2222`
6. **Resolve VM IP**: Read `container list --all --format json`, extract `networks[0].ipv4Address` (format: `192.168.65.x/24`), strip CIDR suffix. The gateway reaches sandbox VMs directly over vmnet.

Sandbox containers are named `openshell-sandbox-<sandbox-name>`. The gateway maps container status to sandbox phases: `running` = Ready, `stopped` = Unknown, anything else = Provisioning.

## Workflow

### Determine Context

Before running commands, establish:

1. **Gateway name**: Default is `openshell`
2. **Config directory**: `~/.openshell/gateways/{name}/`
3. **Gateway port**: Default 8080 (check metadata or server.log for actual port)

### Step 0: Quick Connectivity Check

Run `openshell status` first. This immediately reveals:
- Which gateway and endpoint the CLI is targeting
- Whether the CLI can reach the server (mTLS handshake success/failure)
- The server version if connected

Common errors at this stage:
- **`tls handshake eof`**: The server isn't running or mTLS credentials are missing/mismatched
- **`connection refused`**: The gateway process isn't running or port is wrong
- **`No gateway configured`**: No gateway has been deployed yet

### Step 1: Check Gateway Process

Verify the gateway process is alive:

```bash
# Read the PID file
cat ~/.openshell/gateways/<name>/gateway.pid

# Check if the process is running
ps -p $(cat ~/.openshell/gateways/<name>/gateway.pid 2>/dev/null) 2>/dev/null

# Alternative: search for the process
ps aux | grep openshell-server
```

If the PID file exists but the process is dead, the gateway crashed. Proceed to Step 2 for logs.

If no PID file exists, the gateway was never started or was destroyed. Run `openshell gateway start`.

### Step 2: Check Gateway Logs

Get recent gateway logs to identify startup failures:

```bash
openshell doctor logs --lines 100
```

Or read the log file directly:

```bash
tail -100 ~/.openshell/gateways/<name>/logs/server.log
```

Look for:

- Port binding failures (`address already in use`)
- Certificate/PKI errors (`tls`, `certificate`, `pki`)
- Database errors (`sqlite`, `database`)
- Apple Container CLI errors (`container`, `failed to run`)
- Sandbox creation failures (`failed to create sandbox`, `failed to start`)
- SSH bootstrap failures (`failed to install openssh-server`, `failed to start sshd`)

### Step 3: Check Apple Container Runtime

Verify Apple Container is installed and running:

```bash
# Check runtime status
container system status

# Get version info
container --version

# List all containers (including sandbox VMs)
container list --all --format json
```

If Apple Container is not running:

```bash
container system start
```

If `container` is not found, Apple Container is not installed. It requires macOS with Virtualization.framework support.

### Step 4: Check mTLS / PKI

TLS certificates are generated by `openshell-bootstrap` (using `rcgen`) and stored locally. No Kubernetes secrets are involved.

```bash
# Check if PKI files exist
ls -la ~/.openshell/gateways/<name>/pki/
```

Expected files: `ca.crt`, `tls.crt`, `tls.key`

Common mTLS issues:
- **PKI files missing**: The gateway was deployed but credentials weren't persisted (e.g., interrupted deploy). Destroy and recreate: `openshell gateway destroy <name> && openshell gateway start`.
- **CLI can't connect after redeploy**: Check that the PKI directory contains all three files and that they were updated at deploy time. Compare file timestamps with the gateway start time.
- **Certificate mismatch**: If you manually deleted or replaced PKI files, destroy and recreate the gateway to regenerate a consistent set.

### Step 5: Check Sandbox VMs

List all Apple Container VMs to see sandbox status:

```bash
container list --all --format json
```

Sandbox containers are named `openshell-sandbox-<name>`. Look for:
- **Status**: Should be `running` for active sandboxes.
- **Network**: Each VM should have an IPv4 address on the vmnet bridge (typically `192.168.65.x/24`).

To inspect a specific sandbox:

```bash
# Check if sshd is running inside the sandbox
container exec openshell-sandbox-<name> sh -c 'ps aux | grep sshd'

# Check if port 2222 is listening
container exec openshell-sandbox-<name> sh -c 'ss -tlnp | grep 2222'

# Check the sandbox user exists
container exec openshell-sandbox-<name> sh -c 'id <host-username>'

# Test network connectivity from the sandbox
container exec openshell-sandbox-<name> sh -c 'ip addr show'
```

### Step 6: Check Networking

The gateway uses host networking (it runs natively on macOS). Sandbox VMs get routable IPs on the vmnet bridge.

```bash
# Check what's listening on the gateway port
lsof -i :8080 -sTCP:LISTEN

# Check if the gateway port is reachable
nc -z 127.0.0.1 8080

# Check vmnet networking for sandbox VMs
container list --all --format json | python3 -c "
import json, sys
for c in json.load(sys.stdin):
    name = c.get('configuration', {}).get('id', 'unknown')
    nets = c.get('networks', [])
    ip = nets[0].get('ipv4Address', 'none') if nets else 'none'
    status = c.get('status', 'unknown')
    print(f'{name}: {status}, IP: {ip}')
"
```

Sandbox SSH is on port 2222. The CLI connects to `<vmnet-ip>:2222` using the host username with empty password auth (NSSH1 handshake is skipped for Apple Container sandboxes).

### Step 7: Check Database

The gateway stores state in a SQLite database:

```bash
# Check database file exists and has content
ls -la ~/.openshell/gateways/<name>/data/openshell.db

# Query sandbox state (if sqlite3 is available)
sqlite3 ~/.openshell/gateways/<name>/data/openshell.db ".tables"
sqlite3 ~/.openshell/gateways/<name>/data/openshell.db "SELECT name, phase FROM sandboxes;"
```

## Common Failure Patterns

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| `tls handshake eof` from `openshell status` | Server not running or mTLS credentials missing/mismatched | Check gateway PID (Step 1) and PKI files (Step 4) |
| `connection refused` from `openshell status` | Gateway process not running or port conflict | Check PID file: `cat ~/.openshell/gateways/<name>/gateway.pid && ps -p $(cat ...)` |
| `No gateway configured` | No gateway has been deployed | Run `openshell gateway start` |
| Gateway process dies immediately | Port conflict, binary not found, missing dependencies | Check `server.log` for the error; run `lsof -i :8080` to find port conflicts |
| Port 8080 already in use | Another process bound to the port | Stop conflicting service or use `--port <other>` on `openshell gateway start` |
| Apple Container not running | Service not started | Run `container system start` |
| `container` command not found | Apple Container not installed | Install Apple Container (requires macOS with Virtualization.framework) |
| Sandbox creation fails with "failed to run container CLI" | Apple Container not running or not on PATH | Check `container system status`; ensure `container` is on PATH |
| Sandbox container stuck in "stopped" state | VM crashed or ran out of resources | Check `container list --all --format json`; delete and recreate the sandbox |
| Sandbox image pull fails | Network issue or invalid image reference | Check `container image pull <image>` manually; verify image exists in registry |
| SSH connection refused to sandbox | sshd not running or bootstrap failed | `container exec openshell-sandbox-<name> sh -c 'ps aux \| grep sshd'`; check gateway logs for SSH bootstrap errors |
| SSH auth failure to sandbox | Host user not created in sandbox VM | `container exec openshell-sandbox-<name> sh -c 'id <host-username>'`; check bootstrap_ssh logs |
| Sandbox has no IP address | vmnet networking issue | `container list --all --format json` to check network info; restart Apple Container |
| Sandbox IP unreachable from host | vmnet bridge not routing | Check `container system status`; restart Apple Container with `container system stop && container system start` |
| PKI files missing | Interrupted deploy or manual deletion | `openshell gateway destroy <name> && openshell gateway start` |
| mTLS mismatch after gateway restart | PKI regenerated but CLI still has old certs | Destroy and recreate: `openshell gateway destroy <name> && openshell gateway start` |
| `openshell-server` binary not found | Not built or not on PATH | Build with `cargo build --release -p openshell-server` or set `OPENSHELL_SERVER_BIN` |
| Database locked errors | Multiple gateway processes or stale lock | Check for duplicate processes: `ps aux \| grep openshell-server`; kill stale ones |
| Sandbox `apt-get` fails during SSH bootstrap | No internet from VM or DNS failure | Check vmnet; `container exec openshell-sandbox-<name> sh -c 'ping -c1 8.8.8.8'` |

## Full Diagnostic Dump

Run all diagnostics at once for a comprehensive report:

```bash
echo "=== Connectivity Check ==="
openshell status

echo "=== Gateway PID ==="
PID_FILE=~/.openshell/gateways/openshell/gateway.pid
if [ -f "$PID_FILE" ]; then
  PID=$(cat "$PID_FILE")
  echo "PID: $PID"
  ps -p "$PID" -o pid,stat,start,etime,command 2>/dev/null || echo "Process not running"
else
  echo "No PID file found"
fi

echo "=== Gateway Logs (last 50 lines) ==="
openshell doctor logs --lines 50

echo "=== Apple Container Status ==="
container system status

echo "=== Apple Container Version ==="
container --version

echo "=== All Apple Container VMs ==="
container list --all --format json

echo "=== PKI Files ==="
ls -la ~/.openshell/gateways/openshell/pki/ 2>/dev/null || echo "No PKI directory"

echo "=== Database ==="
ls -la ~/.openshell/gateways/openshell/data/openshell.db 2>/dev/null || echo "No database file"

echo "=== Port 8080 Listeners ==="
lsof -i :8080 -sTCP:LISTEN 2>/dev/null || echo "Nothing listening on 8080"

echo "=== Gateway Process Search ==="
ps aux | grep '[o]penshell-server'

echo "=== Sandbox VM Network Info ==="
container list --all --format json 2>/dev/null | python3 -c "
import json, sys
try:
    for c in json.load(sys.stdin):
        name = c.get('configuration', {}).get('id', 'unknown')
        nets = c.get('networks', [])
        ip = nets[0].get('ipv4Address', 'none') if nets else 'none'
        status = c.get('status', 'unknown')
        print(f'  {name}: status={status}, ip={ip}')
except: print('  (no containers or parse error)')
" 2>/dev/null
```
