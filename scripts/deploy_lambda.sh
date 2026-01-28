#!/usr/bin/env bash
set -euo pipefail

# Simple Lambda deploy helper for Rust (provided.al2023, arm64)
# Usage: ./scripts/deploy_lambda.sh [zip_path] [alias]
# Notes:
# - ZIP must contain a single executable named "bootstrap" at the ZIP ROOT (no subfolder)
# - Binary must be ARM64 (aarch64) for this function

# Config
FN=${FN:-doxle-annotations-api}
# Default alias is "prod" so deployments update the prod alias automatically.
# Override by setting ALIAS to a different name, or to an empty string to skip alias updates.
ALIAS=${ALIAS:-prod}
DESKTOP_ZIP=${DESKTOP_ZIP:-"$HOME/Desktop/bootstrap.zip"}

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEFAULT_BUILD_ZIP="${BE_ROOT}/target/lambda/bootstrap/bootstrap.zip"

# Args (optional)
ZIP=${1:-}
CLI_ALIAS=${2:-}
if [[ -n "${CLI_ALIAS}" ]]; then
  ALIAS="${CLI_ALIAS}"
fi

# Resolve ZIP to default if not provided
if [[ -z "${ZIP}" ]]; then
  ZIP="${DEFAULT_BUILD_ZIP}"
fi

# Force clean and fresh build every deploy
echo "[pre] Cleaning previous build artifacts..."
rm -f "${ZIP}" "${DESKTOP_ZIP}"
cd "${BE_ROOT}"
# Best-effort clean of relevant crates; fallback to full clean if it fails
cargo clean -p doxle-api-lambda -p doxle-shared || cargo clean || true

echo "📦 Building Lambda binary (fresh)..."
echo "Running: cargo lambda build -p doxle-api-lambda --release --arm64 --output-format zip"
cargo lambda build -p doxle-api-lambda --release --arm64 --output-format zip

if [[ ! -f "${ZIP}" ]]; then
  echo "❌ Build failed or ZIP not found at: ${ZIP}" >&2
  exit 1
fi
echo "✅ Build complete!"

# Always copy to Desktop and deploy from there
echo "[0/5] Copying ZIP to Desktop: ${DESKTOP_ZIP}"
mkdir -p "$(dirname "${DESKTOP_ZIP}")"
cp -f "${ZIP}" "${DESKTOP_ZIP}"

echo "[1/5] Deploying code to ${FN} ..."
aws lambda update-function-code \
  --function-name "${FN}" \
  --zip-file "fileb://${DESKTOP_ZIP}" \
  --query 'LastModified' --output text

# Ensure the update has fully propagated before proceeding
aws lambda wait function-updated --function-name "${FN}" || true

echo "[1.5/5] Waiting for Lambda to become Active..."
for i in {1..30}; do
  STATE=$(aws lambda get-function-configuration --function-name "${FN}" --query 'State' --output text 2>/dev/null || echo "Unknown")
  UPDATE_STATUS=$(aws lambda get-function-configuration --function-name "${FN}" --query 'LastUpdateStatus' --output text 2>/dev/null || echo "Unknown")
  
  if [[ "${STATE}" == "Active" && "${UPDATE_STATUS}" == "Successful" ]]; then
    echo "✅ Lambda is Active and ready!"
    break
  fi
  
  echo "  ⏳ Status: ${STATE} | Update: ${UPDATE_STATUS} (${i}/30s)"
  sleep 1
  
  if [[ $i -eq 30 ]]; then
    echo "⚠️  Timeout waiting for Active state, but continuing anyway..."
  fi
done

echo "[2/5] Publishing version ..."
VERSION=$(aws lambda publish-version --function-name "${FN}" --query 'Version' --output text)
echo "Published version: ${VERSION}"

if [[ -n "${ALIAS}" ]]; then
  echo "[3/5] Ensuring alias '${ALIAS}' points to version ${VERSION} ..."
  if aws lambda get-alias --function-name "${FN}" --name "${ALIAS}" >/dev/null 2>&1; then
    aws lambda update-alias \
      --function-name "${FN}" \
      --name "${ALIAS}" \
      --function-version "${VERSION}" \
      --query 'FunctionVersion' --output text
  else
    echo "Alias '${ALIAS}' not found. Creating it..."
    aws lambda create-alias \
      --function-name "${FN}" \
      --name "${ALIAS}" \
      --function-version "${VERSION}" \
      --description "Auto-created by deploy script" \
      --query 'AliasArn' --output text
  fi
  # Show the alias ARN for easy wiring in API Gateway
  ALIAS_ARN=$(aws lambda get-alias --function-name "${FN}" --name "${ALIAS}" --query 'AliasArn' --output text 2>/dev/null || true)
  if [[ -n "${ALIAS_ARN}" ]]; then
    echo "Alias ARN: ${ALIAS_ARN}"
  fi
else
  echo "[3/5] No alias provided, skipping alias update (export ALIAS=prod to enable)"
fi

echo "[4/5] Quick ping (invokes Lambda via API Gateway if reachable) ..."
# Adjust this to any public GET that hits your Lambda; 404 is fine, it still proves the code runs
curl -s -o /dev/null -w "%{http_code}\n" https://api.doxle.ai/invites/test || true

# Show last minute of logs
aws logs tail "/aws/lambda/${FN}" --since 1m || true

echo "Done."
