# caduceusd deployment

This directory contains deployment artifacts for `caduceus-daemon`
(`ops01-deployment-package` per the implementation DAG).

## Service units

| Platform | File | Install path |
|----------|------|--------------|
| Linux (systemd) | `caduceusd.service` | `/etc/systemd/system/caduceusd.service` |
| macOS (launchd) | `com.alexkeagel.caduceusd.plist` | `/Library/LaunchDaemons/com.alexkeagel.caduceusd.plist` |

## Quickstart — Linux

```bash
sudo install -m 0755 target/release/caduceusd /usr/local/bin/caduceusd
sudo install -d -m 0755 /etc/caduceus /var/lib/caduceus /var/log/caduceus
sudo install -m 0644 caduceusd.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now caduceusd
sudo systemctl status caduceusd
```

## Quickstart — macOS

```bash
sudo install -m 0755 target/release/caduceusd /usr/local/bin/caduceusd
sudo install -d /etc/caduceus /var/lib/caduceus /var/log/caduceus
sudo install -m 0644 com.alexkeagel.caduceusd.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.alexkeagel.caduceusd.plist
```

## Configuration

Minimal `caduceusd.toml`:

```toml
workflow_path = "/etc/caduceus/workflow.toml"
workspace_root = "/var/lib/caduceus/workspaces"
poll_interval_ms = 100
max_concurrency = 8
recent_history_ring_size = 32
disconnect_timeout_ms = 60000
disconnect_retention_ms = 3600000
max_dispatch_defer_attempts = 8
```

## Observability (ops02)

Counter metrics exposed via Prometheus text-based exposition format:

- `caduceusd_dispatch_attempts`
- `caduceusd_orphan_reclaim_{success,failure,no_leaf,no_slug}`
- `caduceusd_signal_error` / `caduceusd_reap_timeout` (iter-28 #2-2)

## Release pipeline (ops03)

GitHub Actions workflow at `.github/workflows/caduceusd-release.yml`
builds prebuilt binaries for the v1 target matrix on `v*` tags.
