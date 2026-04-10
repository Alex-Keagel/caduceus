# E2B Behavioral Specification

> Exhaustive specification derived from source reading of `~/caduceus-reference/e2b/`.
> Target: Caduceus AI IDE sandbox execution layer.
> Source commit: HEAD of e2b repository at time of reading.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Authentication](#2-authentication)
3. [SDK API Surface](#3-sdk-api-surface)
4. [Sandbox Lifecycle](#4-sandbox-lifecycle)
5. [Process Execution](#5-process-execution)
6. [PTY (Pseudo-Terminal)](#6-pty-pseudo-terminal)
7. [Filesystem](#7-filesystem)
8. [Networking & Port Forwarding](#8-networking--port-forwarding)
9. [Templates](#9-templates)
10. [Volumes](#10-volumes)
11. [Snapshots](#11-snapshots)
12. [Metrics](#12-metrics)
13. [Git Operations](#13-git-operations)
14. [MCP (Model Context Protocol) Integration](#14-mcp-model-context-protocol-integration)
15. [Streaming & Real-time Output](#15-streaming--real-time-output)
16. [Error Handling](#16-error-handling)
17. [API Specification (OpenAPI)](#17-api-specification-openapi)
18. [Internal Protocol (envd gRPC)](#18-internal-protocol-envd-grpc)
19. [Integration Patterns](#19-integration-patterns)
20. [Configuration Reference](#20-configuration-reference)
21. [Version Compatibility](#21-version-compatibility)

---

## 1. Overview

E2B provides secure micro-VM sandboxes for AI agents. Each sandbox is an isolated Linux environment running in the cloud.

### Architecture

```
┌──────────────────────────────────────────┐
│              Client SDK                   │
│   (TypeScript / Python)                   │
└─────────────┬────────────────────────────┘
              │ HTTPS / gRPC-web
              ▼
┌──────────────────────────────────────────┐
│           E2B API Server                  │
│   https://api.e2b.app                     │
│   - Sandbox lifecycle (REST)              │
│   - Template management (REST)            │
│   - Volume management (REST)              │
└─────────────┬────────────────────────────┘
              │
              ▼
┌──────────────────────────────────────────┐
│         Sandbox VM (micro-VM)             │
│   ┌──────────────────────────────────┐   │
│   │    envd daemon (port 49983)       │   │
│   │  - Filesystem (gRPC-web)          │   │
│   │  - Process/PTY (gRPC-web)         │   │
│   │  - File upload/download (HTTP)    │   │
│   └──────────────────────────────────┘   │
│   ┌──────────────────────────────────┐   │
│   │  mcp-gateway (port 50005)         │   │
│   │  - MCP server proxy               │   │
│   └──────────────────────────────────┘   │
└──────────────────────────────────────────┘
```

### Key Properties
- **Default domain**: `e2b.app`
- **Default API URL**: `https://api.e2b.app`
- **envd port**: `49983`
- **MCP port**: `50005`
- **Default sandbox timeout**: 300 seconds (5 minutes)
- **Max sandbox timeout (Pro)**: 86,400 seconds (24 hours)
- **Max sandbox timeout (Hobby)**: 3,600 seconds (1 hour)
- **Default request timeout**: 60 seconds
- **Keepalive ping interval**: 50 seconds
- **Default template**: `base`
- **Default MCP template**: `mcp-gateway`

---

## 2. Authentication

### API Key Auth
- Header: `X-API-Key: <api_key>`
- Environment variable: `E2B_API_KEY`
- Prefix format: `e2b_***`
- Obtain at: https://e2b.dev/dashboard?tab=keys

### Access Token Auth (Bearer)
- Header: `Authorization: Bearer <access_token>`
- Environment variable: `E2B_ACCESS_TOKEN`
- Used for user-based auth (Supabase-backed)

### Supabase Auth (Internal)
- `X-Supabase-Token` header (user token)
- `X-Supabase-Team` header (team selection)
- Used for dashboard/internal operations

### Admin Auth (Internal)
- Header: `X-Admin-Token`
- Used for administrative operations (node management, bulk kills)

### envd Access Token
- Per-sandbox token returned in sandbox creation response (`envdAccessToken`)
- Sent as `X-Access-Token` header to envd daemon
- Used when `secure: true` (default)

### Traffic Access Token
- Per-sandbox token (`trafficAccessToken`) for accessing sandbox services via proxy
- Only present when `allowPublicTraffic` is not explicitly set to `true`

### User Authentication within Sandbox
- envd uses HTTP Basic Auth: `Authorization: Basic base64(<username>:)`
- No password required, just username
- Default username: `user`
- Available from envd version `0.4.0`; older versions always use `user`

### URL Signature (Secure File Access)
- HMAC-based signatures for file upload/download URLs on secured sandboxes
- Format: `v1_<sha256_base64>`
- Signature input: `{path}:{operation}:{user}:{envdAccessToken}[:{expiration}]`
- Operations: `read` or `write`
- Optional expiration: Unix timestamp in seconds
- URL params: `?signature=v1_...&signature_expiration=<unix_ts>`

---

## 3. SDK API Surface

### TypeScript/JavaScript SDK (`e2b` npm package)

#### Top-Level Exports
```typescript
// Core
export { Sandbox } from './sandbox'
export { Volume, VolumeFileType } from './volume'
export { ApiClient } from './api'
export { ConnectionConfig } from './connectionConfig'
export { Git } from './sandbox/git'

// Template building
export * from './template'
export { ReadyCmd, waitForPort, waitForURL, waitForProcess, waitForFile, waitForTimeout } from './template/readycmd'

// Types
export type { ConnectionOpts, Username } from './connectionConfig'
export type { SandboxOpts, SandboxConnectOpts, SandboxInfo, SandboxMetrics, SandboxState, SandboxListOpts, SandboxNetworkOpts, SandboxLifecycle, SnapshotInfo } from './sandbox/sandboxApi'
export type { CommandResult, CommandHandle, CommandStartOpts, ProcessInfo } from './sandbox/commands'
export type { EntryInfo, WriteInfo, FilesystemEvent, WatchHandle } from './sandbox/filesystem'
export type { McpServer } from './sandbox/mcp'
export type { VolumeInfo, VolumeEntryStat, VolumeWriteOptions } from './volume'

// Error classes
export { SandboxError, TimeoutError, InvalidArgumentError, NotEnoughSpaceError, FileNotFoundError, SandboxNotFoundError, AuthenticationError, GitAuthError, GitUpstreamError, TemplateError, RateLimitError, BuildError, FileUploadError, VolumeError, CommandExitError } from './errors'

// Constants
export { ALL_TRAFFIC } from './sandbox/network'
export { FilesystemEventType } from './sandbox/filesystem/watchHandle'
```

#### Python SDK (`e2b` package)

Two parallel implementations:
- **`AsyncSandbox`** — asyncio-based (`e2b.sandbox_async`)
- **`Sandbox`** — synchronous (`e2b.sandbox_sync`)

Both share the same API surface but with sync vs async signatures.

```python
from e2b import Sandbox, AsyncSandbox
from e2b import Volume, AsyncVolume
```

---

## 4. Sandbox Lifecycle

### 4.1 Creating a Sandbox

#### TypeScript
```typescript
// Default template
const sandbox = await Sandbox.create()

// Specific template
const sandbox = await Sandbox.create('template-name-or-id')

// With options
const sandbox = await Sandbox.create('my-template', {
  timeoutMs: 60_000,          // 60 seconds sandbox lifetime
  metadata: { userId: '123' },
  envs: { NODE_ENV: 'production' },
  secure: true,               // default true — envd secured with token
  allowInternetAccess: true,  // default true
  apiKey: 'e2b_...',
  network: {
    allowPublicTraffic: true,
    allowOut: ['8.8.8.8'],
    denyOut: ['10.0.0.0/8'],
    maskRequestHost: '${PORT}-myapp.example.com',
  },
  lifecycle: {
    onTimeout: 'pause',       // 'kill' (default) or 'pause'
    autoResume: true,         // only when onTimeout = 'pause'
  },
  volumeMounts: {
    '/data': myVolume,        // Volume instance or name string
  },
  mcp: { ... },              // MCP server config
})
```

#### Python
```python
# Sync
with Sandbox.create(
    template="my-template",
    timeout=300,
    metadata={"user_id": "123"},
    envs={"NODE_ENV": "production"},
    secure=True,
    allow_internet_access=True,
    lifecycle={"on_timeout": "pause", "auto_resume": True},
    volume_mounts={"/data": my_volume},
    api_key="e2b_...",
) as sandbox:
    ...

# Async
async with AsyncSandbox.create(template="my-template") as sandbox:
    ...
```

#### REST API Call
```
POST /sandboxes
Headers: X-API-Key: <api_key>
Body:
{
  "templateID": "base",
  "timeout": 300,           // seconds
  "metadata": {},
  "envVars": {},
  "secure": true,
  "allow_internet_access": true,
  "network": { ... },
  "autoPause": false,
  "autoResume": { "enabled": false },
  "mcp": null,
  "volumeMounts": [{ "name": "vol-id", "path": "/data" }]
}
Response:
{
  "sandboxID": "xyz",
  "templateID": "base",
  "clientID": "deprecated",
  "envdVersion": "0.5.7",
  "envdAccessToken": "token",
  "trafficAccessToken": "token",
  "domain": "e2b.app"
}
```

### 4.2 Connecting to an Existing Sandbox

```typescript
// Connect to running or paused sandbox by ID
const sandbox = await Sandbox.connect('sandbox-id', {
  timeoutMs: 60_000,  // updates timeout if longer than existing
})
```

```
POST /sandboxes/{sandboxID}/connect
Body: { "timeout": 300 }
```
- Automatically resumes paused sandboxes.
- Returns same structure as create.

### 4.3 Listing Sandboxes

```typescript
// Paginated
const paginator = Sandbox.list({
  query: {
    metadata: { env: 'production' },
    state: ['running', 'paused'],
  },
  limit: 100,
})

while (paginator.hasNext) {
  const sandboxes = await paginator.nextItems()
  // Each sandbox: SandboxInfo
}
```

```
GET /v2/sandboxes?metadata=<encoded>&state[]=running&limit=100&nextToken=<cursor>
Response: Array of SandboxDetail
Pagination: x-next-token response header
```

### 4.4 Sandbox Info

```typescript
const info = await sandbox.getInfo()
// or static:
const info = await Sandbox.getInfo('sandbox-id', opts)
```

```
GET /sandboxes/{sandboxID}
Response: SandboxDetail {
  sandboxID, templateID, alias, clientID,
  startedAt, endAt, cpuCount, memoryMB, diskSizeMB,
  state, envdVersion, metadata, network, lifecycle,
  volumeMounts, allowInternetAccess
}
```

### 4.5 Timeout Management

```typescript
// Extend or reduce sandbox lifetime
await sandbox.setTimeout(3_600_000)  // 1 hour in ms

// Static
await Sandbox.setTimeout('sandbox-id', 3_600_000, opts)
```

```
POST /sandboxes/{sandboxID}/timeout
Body: { "timeout": 3600 }  // seconds
```

### 4.6 Killing a Sandbox

```typescript
await sandbox.kill()

// Static
const killed = await Sandbox.kill('sandbox-id')
// Returns true if killed, false if not found
```

```
DELETE /sandboxes/{sandboxID}
Response 200: true
Response 404: false
```

### 4.7 Pausing a Sandbox

```typescript
const paused = await sandbox.pause()
// Returns true if paused, false if already paused

// Static
const paused = await Sandbox.pause('sandbox-id')
```

```
POST /sandboxes/{sandboxID}/pause
Response 200: true
Response 409: false (already paused)
Response 404: SandboxNotFoundError
```

### 4.8 Resuming a Sandbox (Resume = Connect)

```typescript
// Resume is done via connect():
const sandbox = await Sandbox.connect('paused-sandbox-id')
```

```
POST /sandboxes/{sandboxID}/connect
Body: { "timeout": 300 }
```

### 4.9 Sandbox State Machine

```
         create()
            │
            ▼
         running ──── setTimeout() ──► running
            │
       kill()│ pause()
            │    │
            ▼    ▼
         killed  paused
                  │
              connect()
                  │
                  ▼
               running
```

### 4.10 Sandbox Lifecycle Configuration

```typescript
lifecycle: {
  onTimeout: 'kill' | 'pause',  // default: 'kill'
  autoResume?: boolean,          // default: false, only when onTimeout='pause'
}
```

When `onTimeout: 'pause'` with `autoResume: true`, sandbox is automatically resumed on next connection.

### 4.11 Health Check

Python SDK exposes `is_running()`:
```python
await sandbox.is_running()  # GET /health on envd, returns True/False
```

---

## 5. Process Execution

### 5.1 Running Commands

Commands run inside `/bin/bash -l -c <cmd>` (login shell).

#### TypeScript
```typescript
// Blocking execution (waits for completion)
const result = await sandbox.commands.run('echo hello')
// result: { exitCode: number, stdout: string, stderr: string, error?: string }

// With options
const result = await sandbox.commands.run('npm install', {
  cwd: '/app',
  user: 'root',
  envs: { NODE_ENV: 'test' },
  onStdout: (data) => console.log('stdout:', data),
  onStderr: (data) => console.error('stderr:', data),
  timeoutMs: 120_000,   // 2 minutes, default 60s
  stdin: false,          // keep stdin open if true (requires envd >= 0.3.0)
  background: false,     // default false
})

// Background execution
const handle = await sandbox.commands.run('long-running-cmd', { background: true })
// handle.pid — process ID
// await handle.wait() — wait for completion (throws CommandExitError on non-zero exit)
// handle.kill() — send SIGKILL
// handle.disconnect() — stop receiving events (process continues)
// handle.stdout — accumulated stdout
// handle.stderr — accumulated stderr
// handle.exitCode — undefined while running, number when done
```

#### Python
```python
# Sync
result = sandbox.commands.run("echo hello")
# result.exit_code, result.stdout, result.stderr

# With options
result = sandbox.commands.run(
    "pip install numpy",
    cwd="/app",
    user="root",
    envs={"PIP_NO_CACHE_DIR": "1"},
    on_stdout=lambda data: print("stdout:", data),
    on_stderr=lambda data: print("stderr:", data),
    timeout=120,          # seconds
    request_timeout=30,   # max time to establish connection
)

# Background
handle = sandbox.commands.run("./server", background=True)
handle.wait()
```

### 5.2 Command Start Process (Internal)

Under the hood, `commands.run()` calls the gRPC `Process.Start`:
```protobuf
StartRequest {
  process: ProcessConfig {
    cmd: "/bin/bash",
    args: ["-l", "-c", "<user_command>"],
    envs: { "KEY": "VALUE" },
    cwd: "/optional/path"
  },
  stdin: false,           // optional bool
  tag: "optional-tag"     // for identifying special commands
}
```

### 5.3 Listing Running Processes

```typescript
const processes = await sandbox.commands.list()
// [{ pid, tag?, cmd, args, envs, cwd? }]
```

```protobuf
// gRPC: Process.List
ListRequest {} → ListResponse { processes: [ProcessInfo] }
```

### 5.4 Sending Stdin

```typescript
// By PID
await sandbox.commands.sendStdin(pid, 'input text\n')
await sandbox.commands.sendStdin(pid, new Uint8Array([...]))

// Via handle
await handle.sendStdin('input\n')
```

```protobuf
SendInput {
  process: { pid: <pid> },
  input: { stdin: <bytes> }
}
```

### 5.5 Closing Stdin

```typescript
// Requires envd >= 0.5.2
await sandbox.commands.closeStdin(pid)
```

### 5.6 Killing a Process

```typescript
// By PID — uses SIGKILL
const killed = await sandbox.commands.kill(pid)  // true if killed, false if not found

// Via handle
await handle.kill()
```

```protobuf
SendSignal {
  process: { pid: <pid> },
  signal: SIGKILL  // or SIGTERM
}
```

### 5.7 Connecting to an Existing Process

```typescript
const handle = await sandbox.commands.connect(pid, {
  onStdout: (data) => console.log(data),
  onStderr: (data) => console.error(data),
  timeoutMs: 60_000,
})
await handle.wait()
```

### 5.8 CommandResult Structure

```typescript
interface CommandResult {
  exitCode: number        // 0 = success
  error?: string          // error message if failed
  stdout: string          // accumulated stdout
  stderr: string          // accumulated stderr
}
```

### 5.9 CommandHandle Structure

```typescript
class CommandHandle {
  pid: number
  exitCode: number | undefined   // undefined while running
  error: string | undefined
  stdout: string                 // accumulated so far
  stderr: string                 // accumulated so far

  async wait(): Promise<CommandResult>  // throws CommandExitError on non-zero exit
  async kill(): Promise<boolean>
  async disconnect(): Promise<void>     // stop receiving, process continues
}
```

### 5.10 Process Selector

Processes can be identified by:
- `pid: number` — process ID
- `tag: string` — named tag (used for start commands in templates)

### 5.11 Default Timeouts

| Operation | Default |
|-----------|---------|
| Command/PTY connection timeout | 60,000 ms |
| Request timeout (API calls) | 60,000 ms |
| Sandbox lifetime | 300,000 ms (5 min) |

---

## 6. PTY (Pseudo-Terminal)

### 6.1 Creating a PTY

```typescript
const pty = await sandbox.pty.create({
  cols: 80,
  rows: 24,
  onData: (data: Uint8Array) => {
    // Raw terminal output
    process.stdout.write(data)
  },
  timeoutMs: 60_000,
  user: 'user',
  envs: { TERM: 'xterm-256color' },  // default: xterm-256color
  cwd: '/home/user',
})
// Returns CommandHandle with pid
```

#### Default PTY Environment
- `TERM=xterm-256color`
- `LANG=C.UTF-8`
- `LC_ALL=C.UTF-8`

#### PTY Shell
Runs `/bin/bash -i -l` (interactive login shell).

### 6.2 Sending Input to PTY

```typescript
// PTY input is raw bytes (not text)
await sandbox.pty.sendInput(pid, new TextEncoder().encode('ls\n'))
await sandbox.pty.sendInput(pid, new Uint8Array([0x04]))  // Ctrl+D (EOF)
```

### 6.3 Resizing PTY

```typescript
await sandbox.pty.resize(pid, { cols: 120, rows: 40 })
```

### 6.4 Killing a PTY

```typescript
const killed = await sandbox.pty.kill(pid)  // SIGKILL
```

### 6.5 Connecting to an Existing PTY

```typescript
const handle = await sandbox.pty.connect(pid, {
  onData: (data) => process.stdout.write(data),
  timeoutMs: 60_000,
})
```

### 6.6 PTY vs Command

| Feature | Command | PTY |
|---------|---------|-----|
| Output type | string (decoded) | Uint8Array (raw) |
| Shell | `/bin/bash -l -c <cmd>` | `/bin/bash -i -l` |
| stdin | text/bytes | raw bytes |
| Close stdin | `closeStdin()` | Ctrl+D (0x04) |
| Resize | No | `pty.resize()` |

---

## 7. Filesystem

All filesystem operations use the envd HTTP API (`/files`) or gRPC (`Filesystem` service).

### 7.1 Reading Files

```typescript
// As string (default)
const text = await sandbox.files.read('/path/to/file')

// As bytes
const bytes = await sandbox.files.read('/path/to/file', { format: 'bytes' })

// As Blob
const blob = await sandbox.files.read('/path/to/file', { format: 'blob' })

// As ReadableStream
const stream = await sandbox.files.read('/path/to/file', { format: 'stream' })

// With gzip decompression
const text = await sandbox.files.read('/path/to/file', { gzip: true })

// As specific user
const text = await sandbox.files.read('/path/to/file', { user: 'root' })
```

HTTP: `GET /files?path=<path>&username=<user>`

### 7.2 Writing Files

```typescript
// Single file
const info = await sandbox.files.write('/path/to/file', 'content')
const info = await sandbox.files.write('/path/to/file', arrayBuffer)
const info = await sandbox.files.write('/path/to/file', blob)
const info = await sandbox.files.write('/path/to/file', readableStream)

// With gzip compression
const info = await sandbox.files.write('/path/to/file', content, { gzip: true })

// As specific user (affects file ownership)
const info = await sandbox.files.write('/path/to/file', content, { user: 'root' })

// Multiple files
const infos = await sandbox.files.write([
  { path: '/file1.txt', data: 'content1' },
  { path: '/file2.txt', data: 'content2' },
])
```

**Write behaviors:**
- Creates file if it doesn't exist
- Overwrites if exists
- Creates intermediate directories automatically

HTTP: `POST /files?path=<path>&username=<user>` (multipart/form-data or octet-stream with envd >= 0.5.7)

#### WriteInfo returned
```typescript
interface WriteInfo {
  name: string
  type?: FileType  // 'file' | 'dir'
  path: string
}
```

### 7.3 Listing Directory

```typescript
const entries = await sandbox.files.list('/path/to/dir')
const entries = await sandbox.files.list('/path/to/dir', {
  depth: 2,   // min 1, default 1; recursion depth
  user: 'root',
})
```

gRPC: `Filesystem.ListDir({ path, depth })`

#### EntryInfo structure
```typescript
interface EntryInfo {
  name: string
  type: FileType         // 'file' | 'dir'
  path: string
  size: number           // bytes
  mode: number           // Unix mode bits
  permissions: string    // e.g. 'rwxr-xr-x'
  owner: string
  group: string
  modifiedTime?: Date
  symlinkTarget?: string // if symlink
}
```

### 7.4 Making Directories

```typescript
const created = await sandbox.files.makeDir('/new/dir/path')
// true if created, false if already exists
```

gRPC: `Filesystem.MakeDir({ path })`

### 7.5 Renaming / Moving

```typescript
const info = await sandbox.files.rename('/old/path', '/new/path')
// Returns EntryInfo for the moved object
```

gRPC: `Filesystem.Move({ source, destination })`

### 7.6 Removing

```typescript
await sandbox.files.remove('/path/to/file')
await sandbox.files.remove('/path/to/dir')  // removes recursively
```

gRPC: `Filesystem.Remove({ path })`

### 7.7 Checking Existence

```typescript
const exists = await sandbox.files.exists('/path/to/check')
// true if exists, false if not
```

gRPC: `Filesystem.Stat` → returns `true` if no NotFound error.

### 7.8 Getting File Info

```typescript
const info = await sandbox.files.getInfo('/path/to/file')
// Returns EntryInfo
```

gRPC: `Filesystem.Stat({ path })`

### 7.9 Watching Directories

```typescript
const handle = await sandbox.files.watchDir(
  '/watched/dir',
  (event) => {
    // event: { name: string, type: FilesystemEventType }
    console.log(event.type, event.name)
  },
  {
    recursive: false,      // requires envd >= 0.1.4
    timeoutMs: 60_000,     // 0 = no timeout
    user: 'user',
    onExit: (err) => console.log('watch ended', err),
  }
)

// Stop watching
handle.stop()
```

gRPC: `Filesystem.WatchDir({ path, recursive })` → streaming

#### FilesystemEventType
```typescript
enum FilesystemEventType {
  Create = 'create',
  Write = 'write',
  Remove = 'remove',
  Rename = 'rename',
  Chmod = 'chmod',
}
```

### 7.10 Upload/Download URLs

```typescript
// Get URL to upload (POST multipart/form-data)
const uploadUrl = await sandbox.uploadUrl('/target/path')
const uploadUrl = await sandbox.uploadUrl('/target/path', {
  user: 'root',
  useSignatureExpiration: 3600,  // seconds; requires secure sandbox
})

// Get URL to download
const downloadUrl = await sandbox.downloadUrl('/source/path')
const downloadUrl = await sandbox.downloadUrl('/source/path', {
  user: 'root',
  useSignatureExpiration: 3600,
})
```

HTTP: `GET /files?path=<path>&username=<user>[&signature=...&signature_expiration=...]`
HTTP: `POST /files?path=<path>&username=<user>[&signature=...]`

### 7.11 Alternative: Non-streaming Watchers (gRPC)

For environments without streaming support:
- `Filesystem.CreateWatcher({ path, recursive })` → `{ watcher_id }`
- `Filesystem.GetWatcherEvents({ watcher_id })` → `{ events: [FilesystemEvent] }`
- `Filesystem.RemoveWatcher({ watcher_id })`

---

## 8. Networking & Port Forwarding

### 8.1 Port Access Pattern

Services running inside the sandbox are accessible externally via:
```
https://{port}-{sandboxId}.{domain}
```

Example: A service on port 3000 in sandbox `abc123` on domain `e2b.app`:
```
https://3000-abc123.e2b.app
```

### 8.2 Getting Host Address

```typescript
const host = sandbox.getHost(3000)
// Returns: "3000-abc123.e2b.app"

// In debug mode:
// Returns: "localhost:3000"
```

Python:
```python
host = sandbox.get_host(3000)
```

### 8.3 Network Configuration

```typescript
const network: SandboxNetworkOpts = {
  allowPublicTraffic: true,          // default true; false = requires trafficAccessToken
  allowOut: ['1.1.1.1', '8.8.0.0/16'],  // CIDR allowlist (takes precedence over denyOut)
  denyOut: ['0.0.0.0/0'],            // CIDR denylist
  maskRequestHost: '${PORT}-myapp.example.com',  // custom host pattern
}
```

- `ALL_TRAFFIC = '0.0.0.0/0'` — convenience constant
- `allowInternetAccess: false` is equivalent to `denyOut: ['0.0.0.0/0']`
- `allowOut` entries always take precedence over `denyOut`

### 8.4 Traffic Access

When `allowPublicTraffic` is false or the sandbox is secured:
- Sandbox URLs are only accessible with `trafficAccessToken`
- Token is returned in sandbox creation/connect response

### 8.5 WebSocket Access

Ports are accessible via both HTTP and WebSocket:
```
wss://{port}-{sandboxId}.{domain}
```

---

## 9. Templates

Templates define custom sandbox environments built from Docker-like instructions.

### 9.1 Template Creation (TypeScript SDK)

```typescript
import { Template, waitForPort, waitForURL } from 'e2b'

const template = Template()
  .fromPythonImage('3.11')
  .runCmd('pip install numpy pandas')
  .copy('requirements.txt', '/app/')
  .setWorkdir('/app')
  .setStartCmd('python server.py', waitForPort(8000))
```

### 9.2 Base Images

```typescript
// Official pre-built images
Template().fromBaseImage()           // e2bdev/base:latest
Template().fromDebianImage('bookworm')
Template().fromUbuntuImage('24.04')
Template().fromPythonImage('3.11')
Template().fromNodeImage('20')
Template().fromBunImage('1.3')

// Custom Docker image
Template().fromImage('myregistry.com/myimage:tag', {
  username: 'user',
  password: 'pass',
})

// AWS ECR
Template().fromAWSRegistry('123.dkr.ecr.us-east-1.amazonaws.com/img:tag', {
  accessKeyId: '...',
  secretAccessKey: '...',
  region: 'us-east-1',
})

// GCP Registry
Template().fromGCPRegistry('gcr.io/project/image:tag', {
  serviceAccountJSON: '...' // path or object
})

// Parse Dockerfile
Template().fromDockerfile('Dockerfile')
Template().fromDockerfile('FROM python:3\nRUN pip install flask')

// Existing E2B template as base
Template().fromTemplate('my-base-template')
```

### 9.3 Builder Methods

```typescript
template
  // Files
  .copy('src', '/dest')
  .copy(['file1', 'file2'], '/dest/', { mode: 0o755 })
  .copyItems([{ src: 'a.txt', dest: '/app/', mode: 0o644 }])
  .remove('/path', { recursive: true, force: true })
  .rename('/old', '/new')
  .makeDir('/new/dir', { mode: 0o755 })
  .makeSymlink('/target', '/link')

  // Commands
  .runCmd('apt-get install vim')
  .runCmd(['cmd1', 'cmd2'])
  .runCmd('npm install', { user: 'root' })

  // Package managers
  .pipInstall('numpy')
  .pipInstall(['pandas', 'scikit-learn'])
  .pipInstall()                  // from current directory
  .pipInstall('numpy', { g: false })  // user-only with --user
  .npmInstall('express')
  .npmInstall(['lodash'], { dev: true })
  .npmInstall('typescript', { g: true })
  .bunInstall('elysia')
  .aptInstall('vim')
  .aptInstall(['git', 'curl'], { noInstallRecommends: true })

  // Environment
  .setEnvs({ NODE_ENV: 'production' })  // build-time only
  .setWorkdir('/app')
  .setUser('root')

  // Git
  .gitClone('https://github.com/user/repo.git', '/app/repo', {
    branch: 'main',
    depth: 1,
  })

  // MCP
  .addMcpServer('exa')           // requires mcp-gateway base image

  // Cache control
  .skipCache()                   // force rebuild from this point

  // DevContainer support
  .betaDevContainerPrebuild('/devcontainer-dir')
  .betaSetDevContainerStart('/devcontainer-dir')

  // Start command (transitions to TemplateFinal)
  .setStartCmd('python app.py', waitForPort(8000))
  .setStartCmd('./server', waitForURL('http://localhost:3000/health', 200))
  .setReadyCmd(waitForProcess('nginx'))
  .setReadyCmd(waitForFile('/tmp/ready'))
  .setReadyCmd(waitForTimeout(5000))  // sleep 5 seconds
```

### 9.4 Building a Template

```typescript
const buildInfo = await Template.build(template, 'my-template:v1.0', {
  tags: ['latest', 'v1.0'],
  cpuCount: 2,
  memoryMB: 1024,
  skipCache: false,
  onBuildLogs: (log) => console.log(log),
  apiKey: 'e2b_...',
})

// buildInfo: { templateId, buildId, name, tags, alias }

// Background build (don't wait for completion)
const buildInfo = await Template.backgroundBuild(template, 'my-template', {
  onBuildLogs: (log) => console.log(log),
})
```

### 9.5 Build Status

```typescript
const status = await Template.getBuildStatus('template-id', {
  buildID: 'build-id',
  logsOffset: 0,
})
// status: 'building' | 'waiting' | 'ready' | 'error'
```

```
GET /templates/{templateID}/builds/{buildID}/status?logsOffset=0&limit=100
GET /templates/{templateID}/builds/{buildID}/logs
```

### 9.6 Template Tags

```typescript
// Assign tags
await Template.assignTags('template-id', ['latest', 'v1.0'])

// Get tags
const tags = await Template.getTemplateTags('template-id')

// Remove tags
await Template.removeTags('template-id', ['v0.9'])
```

### 9.7 Ready Commands

```typescript
// Built-in helpers
waitForPort(8080)              // ss -tuln | grep :8080
waitForURL('http://localhost/health', 200)  // curl check
waitForProcess('nginx')        // pgrep nginx > /dev/null
waitForFile('/tmp/ready')      // [ -f /tmp/ready ]
waitForTimeout(5000)           // sleep 5
```

### 9.8 Template REST API

```
POST /templates                     — Create template (request build)
POST /v2/templates                  — Create template v2
POST /templates/{templateID}        — Trigger build
PATCH /templates/{templateID}       — Update template (public flag, etc.)
PATCH /v2/templates/{templateID}    — Update template v2
DELETE /templates/{templateID}      — Delete template
GET /templates                      — List templates
GET /templates/{templateID}         — Get template info
GET /templates/{templateID}/files/{hash} — Get file upload link
POST /templates/tags                — Assign tags
DELETE /templates/tags              — Remove tags
GET /templates/{templateID}/tags    — List tags
GET /templates/aliases/{alias}      — Check alias exists
```

---

## 10. Volumes

Volumes are persistent storage that survives sandbox deletion and can be mounted into sandboxes.

### 10.1 Creating a Volume

```typescript
const volume = await Volume.create('my-volume', { apiKey: 'e2b_...' })
// volume: { volumeId, name, token, domain, debug }
```

### 10.2 Connecting to a Volume

```typescript
const volume = await Volume.connect('volume-id', opts)
```

### 10.3 Listing Volumes

```typescript
const volumes = await Volume.list(opts)
// [{ volumeId, name }]
```

### 10.4 Destroying a Volume

```typescript
const destroyed = await Volume.destroy('volume-id', opts)
// true if destroyed, false if not found
```

### 10.5 Volume File Operations

```typescript
// List directory
const entries = await volume.list('/path', { depth: 2 })

// Get info
const info = await volume.getInfo('/file.txt')

// Check existence
const exists = await volume.exists('/file.txt')

// Create directory
const stat = await volume.makeDir('/new/dir', {
  uid: 1000, gid: 1000, mode: 0o755, force: true
})

// Read file
const text = await volume.readFile('/file.txt')
const bytes = await volume.readFile('/file.txt', { format: 'bytes' })
const blob = await volume.readFile('/file.txt', { format: 'blob' })
const stream = await volume.readFile('/file.txt', { format: 'stream' })

// Write file
const stat = await volume.writeFile('/file.txt', 'content', {
  uid: 1000, gid: 1000, mode: 0o644, force: true
})

// Remove
await volume.remove('/file.txt')

// Update metadata
const stat = await volume.updateMetadata('/file.txt', { uid: 1000, mode: 0o755 })
```

#### VolumeEntryStat
```typescript
{
  name: string
  path: string
  type: VolumeFileType  // 'unknown' | 'file' | 'directory' | 'symlink'
  size: number
  mode: number
  uid: number
  gid: number
  atime: Date
  mtime: Date
  ctime: Date
}
```

### 10.6 Mounting Volumes into Sandboxes

```typescript
const volume = await Volume.create('my-data')

const sandbox = await Sandbox.create({
  volumeMounts: {
    '/data': volume,          // Volume instance
    '/config': 'config-vol',  // Volume name string
  },
})
```

### 10.7 Volume REST API

```
GET /volumes              — List volumes
POST /volumes             — Create volume: { name }
GET /volumes/{volumeID}   — Get volume info + token
DELETE /volumes/{volumeID} — Delete volume
```

Volume content API (separate endpoint):
```
GET /volumecontent/{volumeID}/dir?path=<path>&depth=<n>
POST /volumecontent/{volumeID}/dir?path=<path>&uid=&gid=&mode=&force=
GET /volumecontent/{volumeID}/file?path=<path>
PUT /volumecontent/{volumeID}/file?path=<path>&uid=&gid=&mode=&force=
GET /volumecontent/{volumeID}/path?path=<path>
PATCH /volumecontent/{volumeID}/path?path=<path>   Body: { uid, gid, mode }
DELETE /volumecontent/{volumeID}/path?path=<path>
```

---

## 11. Snapshots

Snapshots capture the complete state of a running sandbox for later restoration.

### 11.1 Creating a Snapshot

```typescript
// Instance method
const snapshot = await sandbox.createSnapshot()
// snapshot: { snapshotId: string }

// Static
const snapshot = await Sandbox.createSnapshot('sandbox-id', opts)
```

```
POST /sandboxes/{sandboxID}/snapshots
Response: SnapshotInfo { snapshotID, names }
```

**Behavior:** Sandbox is paused while snapshot is being created. Snapshot survives sandbox deletion. The `snapshotId` can be used as a template ID to create new sandboxes.

### 11.2 Creating a Sandbox from Snapshot

```typescript
const newSandbox = await Sandbox.create(snapshot.snapshotId)
```

### 11.3 Listing Snapshots

```typescript
// Instance method (filters by this sandbox's ID)
const paginator = sandbox.listSnapshots({ limit: 50 })

// Static (all snapshots or filtered)
const paginator = Sandbox.listSnapshots({ sandboxId: 'sb-id', limit: 50 })

while (paginator.hasNext) {
  const snapshots = await paginator.nextItems()
}
```

```
GET /snapshots?sandboxID=<id>&limit=100&nextToken=<cursor>
```

### 11.4 Deleting a Snapshot

```typescript
const deleted = await Sandbox.deleteSnapshot('snapshot-id', opts)
```

---

## 12. Metrics

### 12.1 Getting Sandbox Metrics

```typescript
const metrics = await sandbox.getMetrics({
  start: new Date('2024-01-01'),  // optional
  end: new Date(),                // optional
})
// metrics: SandboxMetrics[]
```

```
GET /sandboxes/{sandboxID}/metrics?start=<unix>&end=<unix>
```

#### SandboxMetrics
```typescript
interface SandboxMetrics {
  timestamp: Date
  cpuUsedPct: number    // 0-100%
  cpuCount: number
  memUsed: number       // bytes
  memTotal: number      // bytes
  diskUsed: number      // bytes (requires envd >= 0.2.4)
  diskTotal: number     // bytes
}
```

**Note:** Disk metrics require envd version >= `0.2.4`.

---

## 13. Git Operations

Git operations run inside the sandbox via the `sandbox.git` module (wraps `commands.run`).

### 13.1 Clone

```typescript
const result = await sandbox.git.clone('https://github.com/user/repo.git', {
  path: '/app/repo',     // destination
  branch: 'main',
  depth: 1,              // shallow clone
  username: 'user',      // HTTP auth
  password: 'ghp_...',   // token
  dangerouslyStoreCredentials: false,  // store in git credential helper
  cwd: '/home/user',
  user: 'user',
})
```

### 13.2 Init

```typescript
await sandbox.git.init('/app/repo', {
  bare: false,
  initialBranch: 'main',
})
```

### 13.3 Status

```typescript
const status = await sandbox.git.status('/app/repo')
// status: GitStatus { files: [{ path, status }], ... }
```

### 13.4 Add

```typescript
await sandbox.git.add('/app/repo', {
  files: ['file1.txt', 'file2.txt'],
  all: false,
})
```

### 13.5 Commit

```typescript
await sandbox.git.commit('/app/repo', 'feat: add feature', {
  authorName: 'Bot',
  authorEmail: 'bot@example.com',
  allowEmpty: false,
})
```

### 13.6 Push

```typescript
await sandbox.git.push('/app/repo', {
  remote: 'origin',
  branch: 'main',
  setUpstream: true,
  username: 'user',
  password: 'ghp_...',
})
```

### 13.7 Pull

```typescript
await sandbox.git.pull('/app/repo', {
  remote: 'origin',
  branch: 'main',
  username: 'user',
  password: 'ghp_...',
})
```

### 13.8 Branches

```typescript
const branches = await sandbox.git.branches('/app/repo')
// branches: GitBranches { current, all }
```

### 13.9 Remote Management

```typescript
await sandbox.git.addRemote('/app/repo', 'origin', 'https://github.com/...', {
  fetch: true,
  overwrite: false,
})
```

### 13.10 Reset

```typescript
await sandbox.git.reset('/app/repo', {
  mode: 'hard',     // 'soft' | 'mixed' | 'hard' | 'merge' | 'keep'
  target: 'HEAD',
  paths: ['file.txt'],
})
```

### 13.11 Restore

```typescript
await sandbox.git.restore('/app/repo', {
  paths: ['.'],
  staged: true,    // unstage
  worktree: true,  // restore working tree
  source: 'HEAD',
})
```

### 13.12 Config

```typescript
await sandbox.git.configureUser('Bot Name', 'bot@example.com', {
  scope: 'global',  // 'local' | 'global' | 'system'
  path: '/app/repo', // required for 'local' scope
})

await sandbox.git.setConfig('user.email', 'bot@example.com', opts)
await sandbox.git.getConfig('user.email', opts)
```

### 13.13 Auth Helpers

```typescript
// Temporarily set credentials for a single operation
// (credentials injected into remote URL, then restored)
// Internal, used by push/pull with username/password

// Danger: stores credentials in git credential helper globally
await sandbox.git.dangerouslyAuthenticate({
  username: 'user',
  password: 'ghp_...',
  host: 'github.com',      // default
  protocol: 'https',       // default
})
```

### 13.14 Git Error Handling

- `GitAuthError` — authentication failure (401/403)
- `GitUpstreamError` — no upstream tracking branch
- `InvalidArgumentError` — bad arguments (empty name, missing remote, etc.)
- Git commands run with `GIT_TERMINAL_PROMPT=0` to prevent hanging on prompts

---

## 14. MCP (Model Context Protocol) Integration

### 14.1 Starting MCP in a Sandbox

```typescript
const sandbox = await Sandbox.create({
  mcp: {
    // Named MCP servers (from mcp-server.json catalog)
    exa: { apiKey: 'exa_...' },
    brave: { apiKey: 'brave_...' },

    // GitHub-hosted MCP servers
    'github/my-org/my-mcp-server': {
      runCmd: 'node server.js',
      installCmd: 'npm install',
      envs: { MY_ENV: 'value' },
    },
  },
})
```

### 14.2 MCP Gateway

When `mcp` option is set, E2B:
1. Creates sandbox from `mcp-gateway` template instead of `base`
2. Generates a random UUID as `mcpToken`
3. Starts `mcp-gateway` process as root with config and `GATEWAY_ACCESS_TOKEN`
4. MCP gateway runs on port `50005`

### 14.3 Accessing MCP

```typescript
const url = sandbox.getMcpUrl()
// Returns: "https://50005-{sandboxId}.{domain}/mcp"

const token = await sandbox.getMcpToken()
// Returns UUID token for auth
```

The token is stored in `/etc/mcp-gateway/.token` (readable as root) as fallback.

### 14.4 MCP Server Catalog

The `spec/mcp-server.json` file contains a catalog of pre-configured MCP servers (airtable, aks, apiGateway, etc.) with required environment variables and descriptions.

---

## 15. Streaming & Real-time Output

### 15.1 Process Output Streaming

Output is streamed via gRPC server-side streaming (Connect protocol).

```protobuf
// Process events stream
ProcessEvent {
  oneof event {
    StartEvent { pid: uint32 }
    DataEvent {
      oneof output {
        bytes stdout = 1;
        bytes stderr = 2;
        bytes pty = 3;
      }
    }
    EndEvent { exit_code: int32, exited: bool, status: string, error?: string }
    KeepAlive {}
  }
}
```

### 15.2 Keepalive

Long-running connections send keepalive pings:
- Header: `Keepalive-Ping-Interval: 50` (seconds)
- Server sends `KeepAlive` events to maintain the connection

### 15.3 Command Output Callbacks

```typescript
// Callbacks called for each chunk of output
await sandbox.commands.run('long-cmd', {
  onStdout: (chunk: string) => process.stdout.write(chunk),
  onStderr: (chunk: string) => process.stderr.write(chunk),
})
```

### 15.4 Directory Watch Streaming

```protobuf
WatchDirResponse {
  oneof event {
    StartEvent {}        // signals watch is ready
    FilesystemEvent { name: string, type: EventType }
    KeepAlive {}
  }
}
```

### 15.5 Transport Layer

- **TypeScript SDK**: Uses `@connectrpc/connect-web` with Connect protocol over HTTP/1.1 (gRPC-web compatible)
- **Python SDK**: Uses httpx with custom transport pool for gRPC-web
- **Binary format**: JSON (not protobuf binary) for browser compatibility (`useBinaryFormat: false`)
- **Redirect**: `redirect: 'follow'` patched in to support edge runtimes

---

## 16. Error Handling

### 16.1 TypeScript Error Hierarchy

```
Error
├── SandboxError
│   ├── TimeoutError          — timeout_ms exceeded, sandbox timeout (502)
│   ├── InvalidArgumentError  — bad arguments
│   ├── NotEnoughSpaceError   — disk full
│   ├── NotFoundError (deprecated)
│   │   ├── FileNotFoundError     — file/dir not in sandbox
│   │   └── SandboxNotFoundError  — sandbox doesn't exist
│   ├── TemplateError         — envd version incompatible
│   ├── RateLimitError        — API rate limit
│   └── CommandExitError      — non-zero exit code (implements CommandResult)
├── AuthenticationError       — auth failure
│   └── GitAuthError          — git auth failure
├── GitUpstreamError          — missing upstream tracking
├── BuildError                — template build failed
│   └── FileUploadError       — file upload during build failed
└── VolumeError               — volume operation failed
```

### 16.2 Python Exception Hierarchy

```
Exception
├── SandboxException
│   ├── TimeoutException
│   ├── InvalidArgumentException
│   ├── NotEnoughSpaceException
│   ├── NotFoundException (deprecated)
│   │   ├── FileNotFoundException
│   │   └── SandboxNotFoundException
│   ├── TemplateException
│   ├── RateLimitException
│   └── CommandExitException
├── AuthenticationException
│   └── GitAuthException
├── GitUpstreamException
├── BuildException
│   └── FileUploadException
└── VolumeException
```

### 16.3 gRPC Error Code Mapping

| gRPC Code | Error Type |
|-----------|-----------|
| `InvalidArgument` | `InvalidArgumentError` |
| `Unauthenticated` | `AuthenticationError` |
| `NotFound` | `NotFoundError` / `FileNotFoundError` |
| `Unavailable` | `TimeoutError` (sandbox timeout) |
| `Canceled` | `TimeoutError` (request timeout exceeded) |
| `DeadlineExceeded` | `TimeoutError` (execution timeout) |
| `AlreadyExists` | Returns false (for makeDir) |
| other | `SandboxError` with code prefix |

### 16.4 HTTP Error Codes

| HTTP Code | Error |
|-----------|-------|
| 400 | Bad request |
| 401 | Authentication error |
| 403 | Forbidden |
| 404 | Not found (SandboxNotFoundError, FileNotFoundError) |
| 409 | Conflict (sandbox already paused → returns false) |
| 500 | Server error |
| 502 | Sandbox timeout (mapped to TimeoutError) |

### 16.5 Timeout Behavior

**Sandbox timeout** (`timeoutMs`/`timeout`):
- Sandbox is automatically killed (default) or paused (`onTimeout: 'pause'`) when it expires
- Maximum: 24 hours (Pro), 1 hour (Hobby)
- Default: 300 seconds (5 minutes)
- Can be extended with `setTimeout()` before or after creation

**Request timeout** (`requestTimeoutMs`/`request_timeout`):
- How long SDK waits for API to respond
- Default: 60 seconds
- Applies to each individual HTTP/gRPC call
- Set to `0` to disable

**Command/Watch timeout** (`timeoutMs`/`timeout` in CommandStartOpts):
- How long the streaming connection stays open
- Default: 60 seconds
- Set to `0` to disable for long-running processes
- Causes `DeadlineExceeded` / `TimeoutError` if exceeded

### 16.6 CommandExitError

Thrown by `handle.wait()` when exit code is non-zero:
```typescript
try {
  await sandbox.commands.run('exit 1')
} catch (err) {
  if (err instanceof CommandExitError) {
    console.log(err.exitCode)  // 1
    console.log(err.stdout)    // accumulated stdout
    console.log(err.stderr)    // accumulated stderr
    console.log(err.error)     // error message
  }
}
```

---

## 17. API Specification (OpenAPI)

### 17.1 Base URL

```
https://api.e2b.app
```

### 17.2 Sandbox Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/sandboxes` | Create sandbox |
| `GET` | `/v2/sandboxes` | List sandboxes (paginated) |
| `GET` | `/sandboxes/{sandboxID}` | Get sandbox info |
| `DELETE` | `/sandboxes/{sandboxID}` | Kill sandbox |
| `POST` | `/sandboxes/{sandboxID}/pause` | Pause sandbox |
| `POST` | `/sandboxes/{sandboxID}/resume` | Resume sandbox |
| `POST` | `/sandboxes/{sandboxID}/connect` | Connect to sandbox (resume if paused) |
| `POST` | `/sandboxes/{sandboxID}/timeout` | Set timeout |
| `POST` | `/sandboxes/{sandboxID}/refreshes` | Refresh sandbox |
| `POST` | `/sandboxes/{sandboxID}/snapshots` | Create snapshot |
| `GET` | `/sandboxes/{sandboxID}/logs` | Get logs |
| `GET` | `/sandboxes/{sandboxID}/metrics` | Get metrics |
| `GET` | `/sandboxes/metrics` | Get all sandboxes metrics |

### 17.3 Template Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/templates` | List templates |
| `POST` | `/templates` | Create template |
| `POST` | `/v2/templates` | Create template v2 |
| `GET` | `/templates/{templateID}` | Get template |
| `PATCH` | `/templates/{templateID}` | Update template |
| `PATCH` | `/v2/templates/{templateID}` | Update template v2 |
| `DELETE` | `/templates/{templateID}` | Delete template |
| `POST` | `/templates/{templateID}` | Trigger build |
| `POST` | `/templates/{templateID}/builds/{buildID}` | Trigger specific build |
| `GET` | `/templates/{templateID}/builds/{buildID}/status` | Get build status |
| `GET` | `/templates/{templateID}/builds/{buildID}/logs` | Get build logs |
| `GET` | `/templates/{templateID}/files/{hash}` | Get file upload link |
| `POST` | `/templates/tags` | Assign tags |
| `DELETE` | `/templates/tags` | Remove tags |
| `GET` | `/templates/{templateID}/tags` | List tags |
| `GET` | `/templates/aliases/{alias}` | Check alias exists |

### 17.4 Snapshot Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/snapshots` | List snapshots |

### 17.5 Volume Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/volumes` | List volumes |
| `POST` | `/volumes` | Create volume |
| `GET` | `/volumes/{volumeID}` | Get volume info |
| `DELETE` | `/volumes/{volumeID}` | Delete volume |

### 17.6 Pagination

All paginated endpoints use cursor-based pagination:
- Request: `?limit=100&nextToken=<cursor>`
- Response header: `x-next-token: <next_cursor>` (absent when no more pages)
- Default limit: 100, maximum: 100

### 17.7 NewSandbox Request Body

```yaml
templateID: string (required)
timeout: integer (seconds, required)
metadata: object<string, string>
envVars: object<string, string>
secure: boolean (default: true)
allow_internet_access: boolean (default: true)
network:
  allowPublicTraffic: boolean
  allowOut: string[]
  denyOut: string[]
  maskRequestHost: string
autoPause: boolean (deprecated, use lifecycle)
autoResume:
  enabled: boolean
mcp: object | null
volumeMounts: [{name: string, path: string}]
```

---

## 18. Internal Protocol (envd gRPC)

The `envd` daemon runs inside each sandbox and is the execution engine. It uses the Connect RPC protocol (gRPC-web compatible).

### 18.1 Connection

```
URL: https://{envdPort}-{sandboxId}.{domain}
Port: 49983
Protocol: Connect (gRPC-web)
Auth: X-Access-Token: <envdAccessToken>  (if secure)
      Authorization: Basic base64(<username>:)  (for user context)
Headers: E2b-Sandbox-Id, E2b-Sandbox-Port
```

### 18.2 Process Service

```protobuf
service Process {
  rpc List(ListRequest) returns (ListResponse);
  rpc Connect(ConnectRequest) returns (stream ConnectResponse);
  rpc Start(StartRequest) returns (stream StartResponse);
  rpc Update(UpdateRequest) returns (UpdateResponse);         // resize PTY
  rpc StreamInput(stream StreamInputRequest) returns (StreamInputResponse);
  rpc SendInput(SendInputRequest) returns (SendInputResponse);
  rpc SendSignal(SendSignalRequest) returns (SendSignalResponse);
  rpc CloseStdin(CloseStdinRequest) returns (CloseStdinResponse);  // envd >= 0.5.2
}
```

Key structures:
- `ProcessConfig`: `cmd, args[], envs{}, cwd?`
- `ProcessSelector`: `pid | tag`
- `Signal`: `SIGTERM=15, SIGKILL=9`
- `PTY.Size`: `cols, rows`
- Input: `ProcessInput { stdin: bytes | pty: bytes }`

### 18.3 Filesystem Service

```protobuf
service Filesystem {
  rpc Stat(StatRequest) returns (StatResponse);
  rpc MakeDir(MakeDirRequest) returns (MakeDirResponse);
  rpc Move(MoveRequest) returns (MoveResponse);
  rpc ListDir(ListDirRequest) returns (ListDirResponse);
  rpc Remove(RemoveRequest) returns (RemoveResponse);
  rpc WatchDir(WatchDirRequest) returns (stream WatchDirResponse);

  // Non-streaming watcher API
  rpc CreateWatcher(CreateWatcherRequest) returns (CreateWatcherResponse);
  rpc GetWatcherEvents(GetWatcherEventsRequest) returns (GetWatcherEventsResponse);
  rpc RemoveWatcher(RemoveWatcherRequest) returns (RemoveWatcherResponse);
}
```

### 18.4 envd HTTP API (File Upload/Download)

```
GET  /files?path=<path>&username=<user>[&signature=...&signature_expiration=<ts>]
POST /files?path=<path>&username=<user>[&signature=...]
     Content-Type: multipart/form-data | application/octet-stream (envd >= 0.5.7)
GET  /health
```

### 18.5 envd Version Feature Matrix

| Feature | Min envd Version |
|---------|-----------------|
| Basic process/filesystem | `0.1.0` |
| Recursive directory watch | `0.1.4` |
| Metrics support | `0.1.5` |
| Disk metrics | `0.2.4` |
| `stdin` control (default off) | `0.3.0` |
| Default user header | `0.4.0` |
| `closeStdin` support | `0.5.2` |
| Octet-stream file upload | `0.5.7` |

---

## 19. Integration Patterns

### 19.1 Basic AI Agent Pattern

```typescript
import Sandbox from 'e2b'

const sandbox = await Sandbox.create('base', {
  timeoutMs: 600_000,  // 10 minutes
  envs: { OPENAI_API_KEY: process.env.OPENAI_API_KEY },
})

// Agent writes code
await sandbox.files.write('/app/solution.py', agentGeneratedCode)

// Execute code
const result = await sandbox.commands.run('python /app/solution.py', {
  timeoutMs: 30_000,
  cwd: '/app',
})

if (result.exitCode === 0) {
  console.log('Output:', result.stdout)
} else {
  console.error('Error:', result.stderr)
}

await sandbox.kill()
```

### 19.2 Streaming Output Pattern

```typescript
const sandbox = await Sandbox.create()

let output = ''
const handle = await sandbox.commands.run('npm run build', {
  background: true,
  onStdout: (chunk) => {
    output += chunk
    // Stream to client in real-time
    sendToClient(chunk)
  },
  onStderr: (chunk) => sendErrorToClient(chunk),
  timeoutMs: 300_000,  // 5 minutes for build
})

await handle.wait()
```

### 19.3 Long-lived Sandbox with Keep-alive

```typescript
const sandbox = await Sandbox.create({ timeoutMs: 3_600_000 })

// Periodically extend timeout
const keepAlive = setInterval(async () => {
  await sandbox.setTimeout(3_600_000)
}, 1_800_000)  // every 30 minutes

// ... use sandbox ...

clearInterval(keepAlive)
await sandbox.kill()
```

### 19.4 Sandbox Pause/Resume Pattern

```typescript
// Session 1: create and do work
const sandbox = await Sandbox.create({
  lifecycle: { onTimeout: 'pause' },
  timeoutMs: 300_000,
})
await sandbox.files.write('/work/state.json', JSON.stringify({ step: 1 }))
const sandboxId = sandbox.sandboxId
await sandbox.pause()

// Session 2: resume from paused state
const resumedSandbox = await Sandbox.connect(sandboxId)
const state = JSON.parse(await resumedSandbox.files.read('/work/state.json'))
// state.step === 1
```

### 19.5 Snapshot Pattern

```typescript
// Create base environment
const sandbox = await Sandbox.create()
await sandbox.commands.run('pip install numpy pandas scikit-learn')
await sandbox.files.write('/app/config.json', JSON.stringify(config))

// Snapshot
const snapshot = await sandbox.createSnapshot()
await sandbox.kill()

// Create many sandboxes from snapshot (fast, no reinstallation)
const workers = await Promise.all(
  Array(10).fill(null).map(() => Sandbox.create(snapshot.snapshotId))
)
```

### 19.6 PTY Interactive Session Pattern

```typescript
const sandbox = await Sandbox.create()

const pty = await sandbox.pty.create({
  cols: 220,
  rows: 50,
  onData: (data) => sendToTerminal(data),
})

// Forward terminal input
onTerminalInput((data) => {
  sandbox.pty.sendInput(pty.pid, new TextEncoder().encode(data))
})

// Handle resize
onTerminalResize((cols, rows) => {
  sandbox.pty.resize(pty.pid, { cols, rows })
})
```

### 19.7 File Watch Pattern

```typescript
const sandbox = await Sandbox.create()
await sandbox.files.makeDir('/output')

const watcher = await sandbox.files.watchDir(
  '/output',
  async (event) => {
    if (event.type === 'create' || event.type === 'write') {
      const content = await sandbox.files.read(`/output/${event.name}`)
      processOutput(event.name, content)
    }
  },
  {
    recursive: true,
    timeoutMs: 0,  // no timeout
  }
)

// Run agent
await sandbox.commands.run('./run-agent.sh', { background: true, timeoutMs: 0 })

// Wait for work to complete
await waitForCondition()
watcher.stop()
```

### 19.8 Secure File Sharing Pattern

```typescript
// Secured sandbox
const sandbox = await Sandbox.create({ secure: true })

// Generate upload URL (for external service to upload file)
const uploadUrl = await sandbox.uploadUrl('/uploads/data.csv', {
  useSignatureExpiration: 3600,  // 1 hour
})

// Share download URL (for external service to download result)
const downloadUrl = await sandbox.downloadUrl('/results/output.json', {
  useSignatureExpiration: 1800,  // 30 minutes
})
```

### 19.9 Volume-based Persistent Storage

```typescript
// Create volume once
const volume = await Volume.create('project-data')

// Mount in multiple sandboxes
const sandbox1 = await Sandbox.create({ volumeMounts: { '/data': volume } })
const sandbox2 = await Sandbox.create({ volumeMounts: { '/data': volume } })

// Write from sandbox1, read from sandbox2 (or directly via volume API)
await sandbox1.files.write('/data/result.json', JSON.stringify(result))

const data = await volume.readFile('/result.json')
```

### 19.10 MCP Integration Pattern

```typescript
import Sandbox from 'e2b'

const sandbox = await Sandbox.create({
  mcp: {
    exa: { apiKey: 'exa_...' },
    'github/anthropic/computer-use-demo': {
      runCmd: 'python server.py',
      installCmd: 'pip install -r requirements.txt',
    },
  },
})

const mcpUrl = sandbox.getMcpUrl()
const mcpToken = await sandbox.getMcpToken()

// Connect AI agent to sandbox MCP endpoint
```

### 19.11 Framework Integrations

E2B integrates with:
- **LangChain** — as a code execution tool
- **LlamaIndex** — as a code interpreter
- **CrewAI** — as an agent tool
- **AutoGen** — as an execution environment
- **Vercel AI SDK** — as a streaming execution tool
- **OpenAI Function Calling** — as a tool call handler

Typical pattern: wrap `sandbox.commands.run()` as a tool function.

---

## 20. Configuration Reference

### 20.1 Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `E2B_API_KEY` | API key for authentication | (none) |
| `E2B_ACCESS_TOKEN` | Access token (alternative to API key) | (none) |
| `E2B_DOMAIN` | E2B domain | `e2b.app` |
| `E2B_API_URL` | Override API URL | `https://api.{domain}` |
| `E2B_SANDBOX_URL` | Override sandbox URL | `https://{port}-{id}.{domain}` |
| `E2B_DEBUG` | Enable debug mode (localhost) | `false` |

### 20.2 ConnectionOpts / ApiParams

**TypeScript:**
```typescript
interface ConnectionOpts {
  apiKey?: string
  accessToken?: string
  domain?: string
  apiUrl?: string
  sandboxUrl?: string
  debug?: boolean
  requestTimeoutMs?: number    // default: 60_000
  logger?: Logger
  headers?: Record<string, string>
}
```

**Python:**
```python
class ApiParams(TypedDict, total=False):
    api_key: Optional[str]
    domain: Optional[str]
    api_url: Optional[str]
    debug: Optional[bool]
    request_timeout: Optional[float]   # default: 60.0
    headers: Optional[Dict[str, str]]
    proxy: Optional[ProxyTypes]
    sandbox_url: Optional[str]
```

### 20.3 SandboxOpts

```typescript
interface SandboxOpts extends ConnectionOpts {
  metadata?: Record<string, string>
  envs?: Record<string, string>
  timeoutMs?: number          // default: 300_000
  secure?: boolean            // default: true
  allowInternetAccess?: boolean  // default: true
  mcp?: McpServer
  network?: SandboxNetworkOpts
  volumeMounts?: Record<string, Volume | string>
  sandboxUrl?: string
  lifecycle?: SandboxLifecycle
}
```

### 20.4 Debug Mode

When `debug: true` or `E2B_DEBUG=true`:
- API URL becomes `http://localhost:3000`
- Sandbox URL becomes `http://localhost:{port}`
- Sandbox ID becomes `debug_sandbox_id`
- `kill()` and `setTimeout()` are no-ops

---

## 21. Version Compatibility

### 21.1 SDK Versions

- **TypeScript SDK**: `e2b` npm package
- **Python SDK**: `e2b` Python package

### 21.2 envd Compatibility Checks

The SDK checks envd version on creation and enforces minimum requirements:
- < `0.1.0`: Rejects with `TemplateError` (template rebuild required)
- Specific features gated by version (see §18.5)

### 21.3 Backward Compatibility Notes

- `betaPause()` → use `pause()`
- `betaCreate()` → use `create()`
- `SandboxBetaCreateOpts.autoPause` → use `lifecycle.onTimeout = 'pause'`
- `alias` build option → use `name` parameter
- `logEntries` vs `logs` (deprecated) in build responses
- `NotFoundError` → use `FileNotFoundError` or `SandboxNotFoundError`

### 21.4 Self-Hosting

E2B infrastructure can be self-hosted on AWS or GCP using Terraform.
- Configure via `E2B_DOMAIN`, `E2B_API_URL`, `E2B_SANDBOX_URL`
- Infrastructure code: `github.com/e2b-dev/infra`

---

## Appendix: Key Constants

```typescript
// Ports
envdPort = 49983
mcpPort = 50005

// Timeouts
REQUEST_TIMEOUT_MS = 60_000           // 60s
DEFAULT_SANDBOX_TIMEOUT_MS = 300_000  // 5 min
KEEPALIVE_PING_INTERVAL_SEC = 50      // 50s

// Networks
ALL_TRAFFIC = '0.0.0.0/0'

// envd versions
ENVD_VERSION_RECURSIVE_WATCH = '0.1.4'
ENVD_DEBUG_FALLBACK = '99.99.99'
ENVD_COMMANDS_STDIN = '0.3.0'
ENVD_DEFAULT_USER = '0.4.0'
ENVD_ENVD_CLOSE = '0.5.2'
ENVD_OCTET_STREAM_UPLOAD = '0.5.7'

// Default users
defaultUsername = 'user'
defaultTemplate = 'base'
defaultMcpTemplate = 'mcp-gateway'
defaultBaseImage = 'e2bdev/base'

// File operations
KEEPALIVE_PING_HEADER = 'Keepalive-Ping-Interval'
```
