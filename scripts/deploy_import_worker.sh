#!/usr/bin/env bash
set -euo pipefail

# Build and deploy the import worker Lambda (provided.al2023, arm64).
# Usage: ./scripts/deploy_import_worker.sh [zip_path] [alias]

FN=${FN:-doxle-import-worker}
ALIAS=${ALIAS:-prod}
DESKTOP_ZIP=${DESKTOP_ZIP:-"$HOME/Desktop/bootstrap-import-worker.zip"}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEFAULT_BUILD_ZIP="${BE_ROOT}/target/lambda/bootstrap/bootstrap.zip"

ZIP=${1:-}
CLI_ALIAS=${2:-}
if [[ -n "${CLI_ALIAS}" ]]; then
  ALIAS="${CLI_ALIAS}"
fi
if [[ -z "${ZIP}" ]]; then
  ZIP="${DEFAULT_BUILD_ZIP}"
fi

echo "[pre] Cleaning previous import worker build artifacts..."
rm -f "${ZIP}" "${DESKTOP_ZIP}"
cd "${BE_ROOT}"
cargo clean -p doxle-import-worker || true

echo "📦 Building import worker Lambda..."
echo "Running: cargo lambda build -p doxle-import-worker --release --arm64 --output-format zip"
cargo lambda build -p doxle-import-worker --release --arm64 --output-format zip

if [[ ! -f "${ZIP}" ]]; then
  echo "❌ Build failed or ZIP not found at: ${ZIP}" >&2
  exit 1
fi
echo "✅ Build complete"

echo "[1/4] Copying ZIP to Desktop: ${DESKTOP_ZIP}"
mkdir -p "$(dirname "${DESKTOP_ZIP}")"
cp -f "${ZIP}" "${DESKTOP_ZIP}"

echo "[2/4] Deploying code to ${FN}..."
aws lambda update-function-code \
  --function-name "${FN}" \
  --zip-file "fileb://${DESKTOP_ZIP}" \
  --query 'LastModified' --output text

aws lambda wait function-updated --function-name "${FN}" || true

echo "[3/4] Publishing version..."
VERSION=$(aws lambda publish-version --function-name "${FN}" --query 'Version' --output text)
echo "Published version: ${VERSION}"

if [[ -n "${ALIAS}" ]]; then
  echo "[4/4] Pointing alias '${ALIAS}' to version ${VERSION}..."
  if aws lambda get-alias --function-name "${FN}" --name "${ALIAS}" >/dev/null 2>&1; then
    aws lambda update-alias \
      --function-name "${FN}" \
      --name "${ALIAS}" \
      --function-version "${VERSION}" \
      --query 'FunctionVersion' --output text
  else
    aws lambda create-alias \
      --function-name "${FN}" \
      --name "${ALIAS}" \
      --function-version "${VERSION}" \
      --description "Auto-created by deploy script" \
      --query 'AliasArn' --output text
  fi
  ALIAS_ARN=$(aws lambda get-alias --function-name "${FN}" --name "${ALIAS}" --query 'AliasArn' --output text 2>/dev/null || true)
  if [[ -n "${ALIAS_ARN}" ]]; then
    echo "Worker alias ARN: ${ALIAS_ARN}"
  fi
else
  echo "[4/4] No alias provided; skipping alias update"
fi

echo "Done."
