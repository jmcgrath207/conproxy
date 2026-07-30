---
name: k8s-dev
description: >
  Troubleshoot conproxy kind + Tilt + Helm dev loop. Port strategy,
  image build, HOST_IP, backends-on-host, dashboard, e2e-k8s.
  Invoke on tilt/kind failures or "address already in use".
license: MIT
metadata:
  repo: conproxy
  scope: k8s dev loop troubleshooting
---

# K8s Dev Loop Troubleshooting

## When to Apply

Load this skill when:

- `tilt up` fails with `UpdateError` or image build
- Port-forward gives `address already in use` on 9999/10000
- conproxy pod is Running but `/health` or `/dashboard` unreachable
- Cascade returns 404 / empty results from backends
- Helm chart values don't apply (`hostIP`, `webUi`)
- `corpus-seed` or `e2e-k8s` fail in Tilt UI
- Any kind cluster lifecycle question
- Need a full teardown + restart: `make dev-restart`

## Dev lifecycle targets

The Makefile provides three lifecycle targets for the kind + Tilt dev loop:

| Target | What it does |
|--------|-------------|
| `make dev-up` | kind cluster → `tilt up` (foreground). Fast path; assumes backends already seeded. |
| `make dev-down` | `tilt down` → free ports 9999/10000 → `kind delete cluster` → `docker compose down -v` |
| `make dev-restart` | Full cycle: down → fresh kind → backends up → `corpus_seed --clear` → `tilt up` |
| `make devex` | DevEx auto-smoke: drive opencode-test (port 14096) with MCP-only prompts, random product. Default model `opencode/big-pickle` (free, no key). |
| `make devex-attach` | `docker exec -it opencode-test opencode -s $DEVEX_SESSION` |
| `make devex-status` | Print sticky `DEVEX_SESSION` + last smoke result |
| `make devex-new` | Mint a fresh session on next smoke |

These call scripts in `scripts/` (`dev-up.sh`, `dev-down.sh`, `dev-restart.sh`,
`devex-smoke.sh`, `devex-session.sh`). Run from the repo root or via make.

For corpus quality or template edits, run `cargo run --bin corpus_gen` to
regenerate JSONL after changing `tests/corpus/templates/` files, then
re-seed: `make dev-restart`.

## Architecture (locked)

```
  Host (Linux)                        kind cluster "conproxy"
  ┌──────────────────────┐            ┌──────────────────────┐
  │  tilt up             │  PF        │  conproxy:deployment  │
  │  port_forwards ──────┼───────────►│  pod:9999 (gRPC)     │
  │  9999:9999           │            │  pod:10000 (HTTP)    │
  │  10000:10000         │            │  /dashboard          │
  │  links→/dashboard    │            │  configmap (conproxy)│
  │                      │            │  service (NodePort)  │
  │  docker compose      │  hostIP    │                      │
  │  qdrant:6333 ←───────┼────────────┤  upstreams point     │
  │  es:9200             │            │  at hostIP:PORT      │
  │  os:9201             │            │  pod→host IP gateway │
  │  meili×2:7700/7701   │            │                      │
  │  pgvector:5432       │            │                      │
  └──────────────────────┘            └──────────────────────┘
```

**Locked decisions (do not change without user sign-off):**

- Kind has **no** `extraPortMappings` for 9999/10000 — Tilt owns those via `port_forwards`
- Tiltfile uses `custom_build` (not `docker_build`) — no `tag=` kwarg in Tilt 0.37.5
- Helm `helm()` uses `set=['k=v']` list — no `values=dict` kwarg in Tilt 0.37.5
- Backends run on host via compose, not in cluster
- `HOST_IP` detection order: bridge gateway → kind gateway → `172.17.0.1`
- Web UI served at `/dashboard`, embedded via `rust-embed`, requires `ui/` folder at compile time

## Decision Tree

### Symptom → Check → Fix

| Symptom | Check | Fix |
|---------|-------|-----|
| `address already in use` 9999/10000 | `ss -ltnp \| grep -E '9999\|10000'`; kind cluster was created with old `extraPortMappings` config | `./scripts/kind-down.sh && ./scripts/kind-up.sh` — recreates cluster without extraPortMappings |
| Docker build fails: `no function `get` on Assets` | Dockerfile has `COPY ui ./ui`? | Add `COPY ui ./ui` before final `cargo build` (rust-embed reads folder at compile time) |
| Tiltfile load error: `unknown kwargs: tag` | Using `docker_build(tag=...)` | Switch to `custom_build` (Tilt 0.37.x rejects `tag=` on `docker_build`) |
| Tiltfile load error: `unknown kwargs: values` | Using `helm(values=...)` | Switch to `helm(..., set=['k=v'])` list format |
| conproxy build trigger ignores UI changes | `custom_build` `deps` missing `'ui/'` | Add `'ui/'` to `deps` list in Tiltfile |
| Pod Ready but /health unreachable | `tilt get uiresource conproxy -o yaml \| grep -A5 buildHistory` | Wait for PF reconnect; check `kubectl get pods` is Running; check `kubectl logs deploy/conproxy` for listen addr |
| `upstream network error` / ES 404 | `kubectl logs deploy/conproxy \| grep -i error`; corpus seeded? | Run `corpus-seed` local resource manually in Tilt UI or `cargo run --bin corpus_seed --features embed,pgvector -- --corpus all` |
| Postgres `builder error for url` | `values.yaml` `type: pgvector` present? Image built with `--features release` (includes pgvector)? | Verify feature flag; rebuild image |
| `/dashboard` returns 404 | `curl -sSI http://127.0.0.1:10000/dashboard/index.html`; check `tilt logs conproxy` for route mount | Ensure `[proxy.web_ui] enabled = true` in configmap; image must have `ui/` folder at compile time |
| Backends unreachable from pod | `kubectl exec deploy/conproxy -- curl -s http://<hostIP>:6333/health` | Tilt prints `Host gateway IP:` at startup; verify matches `docker network inspect bridge \| grep Gateway` |
| Tilt dashboard link broken | Tiltfile `k8s_resource` has `links=`? | Add `links=['http://127.0.0.1:10000/dashboard']` to `k8s_resource('conproxy', ...)` |
| Stale image after src changes | `custom_build` `deps` list includes `'src/'`, `'ui/'`, `'Cargo.*'`? | Deps must cover all sources the build uses; rust-embed needs `ui/` trigger |
| `no associated function `get` found for struct `Assets`` | `ui/` folder missing at compile time | `COPY ui ./ui` in Dockerfile; ensure `ui/` dir exists locally |

## Diagnostic Commands

```bash
# Resource status
tilt get uiresources
tilt get uiresource conproxy -o yaml   # buildHistory.error, runtimeStatus, updateStatus
tilt get uiresource '(Tiltfile)' -o yaml

# Logs
tilt logs conproxy     # latest build + pod logs
tilt logs              # all resources
tilt logs --since=5m

# Kind
kind get clusters
kind get nodes --name conproxy
kind export logs --name conproxy /tmp/kind-logs

# Kubernetes
kubectl get pods,svc,cm -l app.kubernetes.io/name=conproxy
kubectl describe pod -l app.kubernetes.io/name=conproxy
kubectl logs deploy/conproxy --tail=80
kubectl exec deploy/conproxy -- curl -s http://127.0.0.1:10000/health

# Network debugging
ss -ltnp | grep -E '9999|10000'
docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}'
docker network inspect kind --format '{{range .IPAM.Config}}{{.Gateway}} {{end}}'
docker inspect conproxy-control-plane | jq '.[0].NetworkSettings.Ports'
curl -sS http://127.0.0.1:10000/health
curl -sSI http://127.0.0.1:10000/dashboard
curl -sS http://127.0.0.1:10000/dashboard/index.html

# Helm template (dry-run)
helm template conproxy deploy/helm/conproxy/ --set hostIP=172.18.0.1 --set image.repository=conproxy --set image.tag=dev
```

## Healthy Loop Checklist

After each `tilt up`, verify in order:

1. [ ] Tiltfile loads without errors (check Tilt UI or `tilt get uiresource '(Tiltfile)'`)
2. [ ] `backends-up` → `backends-wait` complete (OK status)
3. [ ] `conproxy` image builds without errors (check build log for `Assets::get` / rust-embed errors)
4. [ ] `conproxy` pod transitions: Scheduled → Initialized → Ready
5. [ ] Port-forwards green: 9999 (gRPC), 10000 (HTTP/dashboard)
6. [ ] `curl -s http://127.0.0.1:10000/health` returns 200
7. [ ] `curl -sSI http://127.0.0.1:10000/dashboard` returns 200
8. [ ] Tilt UI shows clickable `web-ui` link → opens dashboard
9. [ ] (optional) Trigger `corpus-seed` → green
10. [ ] (optional) Trigger `e2e-k8s` → all tests pass

## File Map

| File | Purpose |
|------|---------|
| `Tiltfile` | Dev loop definition: build, deploy, local resources, PFs, links |
| `deploy/tilt/kind-config.yaml` | kind config — **no** extraPortMappings for app ports |
| `deploy/helm/conproxy/Chart.yaml` | Helm chart metadata |
| `deploy/helm/conproxy/values.yaml` | Chart defaults: images, ports, upstreams, webUi, probes |
| `deploy/helm/conproxy/templates/configmap.yaml` | Renders `conproxy.toml` from values; includes `[proxy.web_ui]` |
| `deploy/helm/conproxy/templates/deployment.yaml` | Pod template with resource limits, probes, configmap volume |
| `deploy/helm/conproxy/templates/service.yaml` | NodePort service (31999 gRPC, 31000 HTTP) |
| `scripts/kind-up.sh` | Creates kind cluster, exports `HOST_IP` |
| `scripts/kind-down.sh` | Deletes kind cluster |
| `scripts/backends-wait.sh` | Waits for all host backends to pass health checks |
| `scripts/e2e-k8s.sh` | Runs e2e tests against cluster deployment |
| `Dockerfile` | Multi-stage build — must `COPY ui ./ui` before cargo build |
| `ui/` | Web UI frontend files, embedded at compile time by `rust-embed` |
| `docs/k8s-dev.md` | Human-readable dev loop docs |
| `src/proxy/server/web_ui.rs` | Rust-embed Assets struct + SPA handler |

## Don'ts

- Don't add `extraPortMappings` for 9999/10000 in kind config — Tilt owns those
- Don't use `docker_build` with `tag=` kwarg — Tilt 0.37.5 rejects it; use `custom_build`
- Don't use `helm(..., values=dict(...))` — Tilt 0.37.5 rejects `values=`; use `set=['k=v']`
- Don't forget to `COPY ui ./ui` when adding/changing dashboard files
- Don't expect e2e tests to pass without running `corpus-seed` first
- Don't build image with `--features pgvector` missing if `values.yaml` has `type: pgvector`
- Don't mix kind `extraPortMappings` and Tilt `port_forwards` — recreate cluster to fix

## Pointers

- `docs/k8s-dev.md` — human-readable dev loop docs
- `contributing` skill — e2e test vertical, test_runner, full test matrix
- `AGENTS.md` §Known Gaps — port strategy summary, embed gaps
