#
# Tilt dev loop for conproxy on kind.
#
# Prerequisites:
#   ./scripts/kind-up.sh                           # creates kind cluster + exports HOST_IP
#   docker compose -f tests/e2e/docker-compose.yml up -d   # start backends on host
#
# Start:
#   tilt up
#
# Stop:
#   tilt down
#
# After tilt up, conproxy reachable at:
#   curl http://127.0.0.1:10000/health
#   grpcurl -plaintext 127.0.0.1:9999 list
#   open http://127.0.0.1:10000/dashboard  # web UI
#

# ---------------------------------------------------------------------------
# 1. Detect docker host IP (used by conproxy in-cluster to reach backends
#    running on the host). Falls back to bridge gateway → kind gateway → 172.17.0.1.
#    Order matches scripts/kind-up.sh (bridge first, then kind).
# ---------------------------------------------------------------------------

HOST_IP = str(local(
    "docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null || "
    + "docker network inspect kind --format '{{range .IPAM.Config}}{{.Gateway}} {{end}}' 2>/dev/null "
    + "| tr ' ' '\\n' | grep '\\.' | head -1 || "
    + "echo '172.17.0.1'"
)).strip()

print('Host gateway IP: ' + HOST_IP + ' (conproxy in-cluster → host backends)')

# ---------------------------------------------------------------------------
# 2. Build conproxy image
# ---------------------------------------------------------------------------

custom_build(
    'conproxy',
    'docker build -t $EXPECTED_REF . && kind load docker-image $EXPECTED_REF --name conproxy',
    deps=['src/', 'ui/', 'Cargo.toml', 'Cargo.lock', 'build.rs', 'proto/', 'sdk/', 'tests/'],
    tag='dev',
    skips_local_docker=True,
)

# ---------------------------------------------------------------------------
# 3. Install conproxy via Helm chart
# ---------------------------------------------------------------------------

helm_values_override = [
    'hostIP=' + HOST_IP,
    'image.repository=conproxy',
    'image.tag=dev',
]

k8s_yaml(helm(
    'deploy/helm/conproxy/',
    name='conproxy',
    set=helm_values_override,
))

# ---------------------------------------------------------------------------
# 4. conproxy resource (port-forwards for gRPC + HTTP)
# ---------------------------------------------------------------------------

k8s_resource(
    'conproxy',
    port_forwards=['9999:9999', '10000:10000'],
    links=['http://127.0.0.1:10000/dashboard'],
)

# ---------------------------------------------------------------------------
# 5. Local resources: backends via docker compose, corpus seed, e2e tests
# ---------------------------------------------------------------------------

# Bring up the backends on the host (qdrant, elastic, opensearch, meili×2, pgvector).
# Tilt only prints logs from this if the user wants to follow them.
local_resource(
    'backends-up',
    cmd='docker compose -f tests/e2e/docker-compose.yml up -d',
    auto_init=True,
)

# Wait for backends to be reachable before proceeding to seed.
local_resource(
    'backends-wait',
    cmd='./scripts/backends-wait.sh',
    resource_deps=['backends-up'],
)

# Seed the corpora (docs/tickets/code, overlap) into all 5 backends with
# real ONNX MiniLM embeddings. Requires the --embed cargo build.
local_resource(
    'corpus-seed',
    cmd='cargo run --bin corpus_seed --features embed,pgvector -- --corpus all --host http://localhost',
    resource_deps=['backends-wait'],
    trigger_mode=TRIGGER_MODE_MANUAL,
)

# Run e2e tests against the live cluster (proxy via port-forward,
# backends already up). Results land in tests/results/e2e-tilt/<ts>/
# + test_runner index.html.
local_resource(
    'e2e-k8s',
    cmd='PROXY_URL=http://127.0.0.1:10000 QDRANT_URL=http://localhost:6333 '
        + 'ELASTIC_URL=http://localhost:9200 OPENSEARCH_URL=http://localhost:9201 '
        + 'MEILI1_URL=http://localhost:7700 MEILI2_URL=http://localhost:7701 '
        + 'PGVECTOR_URL=postgres://postgres:postgres@localhost:5432/conproxy_test '
        + 'E2E_EXTERNAL_PROXY=1 ./scripts/e2e-k8s.sh',
    resource_deps=['corpus-seed', 'conproxy'],
    trigger_mode=TRIGGER_MODE_MANUAL,
    auto_init=False,
)

# ---------------------------------------------------------------------------
# 6. opencode test container (host docker, --network host) for MCP testing.
#    Runs `opencode serve` on port 14096 (host network → also on host).
#    NO bind mount of opencode's session DB: every container recreate
#    starts with an empty in-container session store (fresh sessions on
#    each rebuild / dev-restart). The host sticky SID (.conproxy/devex-session)
#    is cleared on every recreate so it never points at a dead session.
#    Optional model credentials are passed through from the host env (NO
#    host auth.json mount). Default model is `opencode/big-pickle` (free,
#    no key required) so it works out of the box; override with
#    DEVEX_MODEL=opencode/<other>.
#    Auto-starts on `tilt up` (DEVEX_OPENCODE_AUTO=1 by default); set
#    DEVEX_OPENCODE_AUTO=0 to require a manual trigger.
#    Human resume:  make devex-attach   (uses .conproxy/devex-session)
# ---------------------------------------------------------------------------

DEVEX_OPENCODE_PORT = os.environ.get('DEVEX_OPENCODE_PORT', '14096')
# Default ON: opencode-test auto-starts so `devex-smoke` (and human handoff)
# work out of the box. Set DEVEX_OPENCODE_AUTO=0 to keep it manual.
DEVEX_OPENCODE_AUTO = os.environ.get('DEVEX_OPENCODE_AUTO', '1') == '1'

# Ensure .conproxy exists for the sticky SID file (no opencode-data dir needed).
local('mkdir -p .conproxy')

# Build the optional env passthrough — only forward what is set on the host.
# We do NOT mount any host opencode state (no auth, no session DB).
def _shell_quote(s):
    return "'" + s.replace("'", "'\\''") + "'"

OPENCODE_ENV_FLAGS = ''
for _name in ('OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'GOOGLE_API_KEY',
              'MISTRAL_API_KEY', 'GROQ_API_KEY', 'OPENCODE_SERVER_PASSWORD',
              'OPENCODE_SERVER_USERNAME', 'OPENCODE_DISABLE_TELEMETRY'):
    _val = os.environ.get(_name, '')
    if _val:
        OPENCODE_ENV_FLAGS += ' -e ' + _name + '=' + _shell_quote(_val)

# Each recreate: clear the host sticky SID so it can't point at a dead session.
# The container's opencode session DB is in-container only (no bind mount),
# so a fresh container always starts with no sessions.
OPENCODE_RUN_CMD = (
    'docker build -t opencode-test -f Dockerfile.opencode . '
    + '&& (docker rm -f opencode-test 2>/dev/null || true) '
    + '&& rm -f "$(pwd)/.conproxy/devex-session" '
    + '&& docker run -d --network host --name opencode-test '
    + OPENCODE_ENV_FLAGS + ' '
    + '-e OPENCODE_PORT=' + DEVEX_OPENCODE_PORT + ' '
    + 'opencode-test'
)

local_resource(
    'opencode-test',
    cmd=OPENCODE_RUN_CMD,
    deps=['Dockerfile.opencode', 'opencode.json', 'conproxy.toml'],
    resource_deps=['conproxy'],
    trigger_mode=TRIGGER_MODE_MANUAL if not DEVEX_OPENCODE_AUTO else TRIGGER_MODE_AUTO,
    auto_init=DEVEX_OPENCODE_AUTO,
)

# ---------------------------------------------------------------------------
# 7. DevEx auto-smoke — drives the running opencode-test container with
#    MCP-only prompts. Picks a random product from the corpus; asserts no
#    401 / UNAUTHENTICATED, prints the session id (DEVEX_SESSION) for
#    handoff. Skipped if no model credentials are visible to the container.
# ---------------------------------------------------------------------------

DEVEX_SMOKE_AUTO = os.environ.get('DEVEX_SMOKE_AUTO', '1') == '1'
DEVEX_MODEL = os.environ.get('DEVEX_MODEL', 'opencode/big-pickle')

local_resource(
    'devex-smoke',
    cmd='DEVEX_OPENCODE_PORT=' + DEVEX_OPENCODE_PORT + ' '
        + 'DEVEX_MODEL=' + DEVEX_MODEL + ' '
        + './scripts/devex-smoke.sh',
    deps=['scripts/devex-smoke.sh', 'scripts/devex-session.sh'],
    resource_deps=['opencode-test', 'corpus-seed', 'conproxy'],
    trigger_mode=TRIGGER_MODE_MANUAL if not DEVEX_SMOKE_AUTO else TRIGGER_MODE_AUTO,
    auto_init=DEVEX_SMOKE_AUTO,
)
