set -euo pipefail
: "${CLARION_DEMO_PROGRAM_ID:?repository variable CLARION_DEMO_PROGRAM_ID is not set}"
: "${BUILD_HASH:?BUILD_HASH is not set}"
ENFORCE="${ENFORCE:-true}"
RPC_URL="${RPC_URL:-https://api.devnet.solana.com}"
summary() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "$1" >> "$GITHUB_STEP_SUMMARY"
  fi
  echo "$1"
}
report() {
  if [ "$ENFORCE" = "true" ]; then
    echo "::error::$1"
    exit 1
  fi
  echo "::warning::$1"
}
deployed=""
for attempt in 1 2 3 4 5; do
  if deployed="$(solana-verify --url "$RPC_URL" get-program-hash "$CLARION_DEMO_PROGRAM_ID")"; then
    break
  fi
  deployed=""
  sleep $((attempt * 10))
done
if [ -z "$deployed" ]; then
  report "could not read the hash of the demo program $CLARION_DEMO_PROGRAM_ID from $RPC_URL"
elif [ "$deployed" = "$BUILD_HASH" ]; then
  summary "demo program $CLARION_DEMO_PROGRAM_ID matches the build hash $deployed"
else
  summary "demo program $CLARION_DEMO_PROGRAM_ID runs $deployed, the build is $BUILD_HASH"
  report "the demo program hash differs from the build hash"
fi
